//! `jump_clone_names` — persisted labels for jump clones (`jumpCloneSvc.SetJumpCloneName`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

/// Private JSON side-store key in `item_custom_names` (jumpCloneID -> label).
pub(crate) const JUMP_CLONE_NAMES_STORE_KEY: i64 = -10_000_006;

/// DERIVED: maximum label length accepted. The client UI limit is not available in the SDE;
/// 100 characters is an assumption.
pub(crate) const MAX_JUMP_CLONE_NAME_LEN: usize = 100;

fn load(snap: &eve_db::WorldStateSnapshot) -> HashMap<i64, String> {
    snap.item_custom_names
        .get(&JUMP_CLONE_NAMES_STORE_KEY)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

/// Label of a jump clone (empty string when none was set).
pub fn name_of(snap: &eve_db::WorldStateSnapshot, jump_clone_id: i64) -> String {
    load(snap).get(&jump_clone_id).cloned().unwrap_or_default()
}

/// `SetJumpCloneName(cloneID, newName)`: only the clone's owner may rename it; an empty name clears it.
pub fn set_jump_clone_name(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let clone_id = args.first().and_then(|a| a.as_i64()).ok_or_else(|| BridgeRpcError {
        code: "InvalidJumpCloneID".into(),
        message: "SetJumpCloneName requires cloneID".into(),
    })?;
    let name = args.get(1).and_then(|a| a.as_str()).unwrap_or("").trim().to_string();
    if name.chars().count() > MAX_JUMP_CLONE_NAME_LEN {
        return Err(BridgeRpcError {
            code: "JumpCloneNameTooLong".into(),
            message: format!("Jump clone names are limited to {MAX_JUMP_CLONE_NAME_LEN} characters"),
        });
    }
    ctx.db.try_with_mut_snapshot(|snap| {
        let owns = snap.jump_clones.get(&char_id).is_some_and(|l| l.iter().any(|c| c.jump_clone_id == clone_id));
        if !owns {
            return Err(BridgeRpcError {
                code: "JumpCloneNotFound".into(),
                message: format!("Jump clone {clone_id} not found"),
            });
        }
        let mut names = load(snap);
        if name.is_empty() {
            names.remove(&clone_id);
        } else {
            names.insert(clone_id, name.clone());
        }
        if let Ok(json) = serde_json::to_string(&names) {
            snap.item_custom_names.insert(JUMP_CLONE_NAMES_STORE_KEY, json);
        }
        Ok(())
    })?;
    Ok(PyRep::None)
}

/// `ValidateInstallJumpClone`: validates prerequisites (docked, clone capacity, funds)
/// for installing a jump clone at the current station or structure.
pub fn validate_install_jump_clone(
    ctx: &ServerContext,
    session: &SessionState,
    _args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_none() && session.structure_id.is_none() {
        return Err(BridgeRpcError {
            code: "MustBeDocked".into(),
            message: "ValidateInstallJumpClone requires being docked at an NPC station or structure".into(),
        });
    }
    let (clone_count, max_clones, balance) = ctx.db.with_snapshot(|snap| {
        let count = snap.jump_clones.get(&char_id).map(|v| v.len()).unwrap_or(0);
        let bal = snap.characters.get(&char_id).map(|c| c.balance.0).unwrap_or(0);
        let skills = snap.skills.get(&char_id);
        let info_lvl = skills.and_then(|m| m.get(&24242)).map(|s| s.level as usize).unwrap_or(0);
        let adv_lvl = skills.and_then(|m| m.get(&33407)).map(|s| s.level as usize).unwrap_or(0);
        let elite_lvl = skills.and_then(|m| m.get(&73910)).map(|s| s.level as usize).unwrap_or(0);
        (count, info_lvl + adv_lvl + elite_lvl, bal)
    });
    // Canonical jump clone installation fee: 900,000 ISK = 90,000,000 cents
    let clone_cost_cents = 90_000_000;
    if balance < clone_cost_cents {
        return Err(BridgeRpcError {
            code: "NotEnoughMoney".into(),
            message: "Insufficient funds to install jump clone (900,000 ISK required)".into(),
        });
    }
    if clone_count >= max_clones {
        return Err(BridgeRpcError {
            code: "JumpCloneLimitReached".into(),
            message: format!(
                "Maximum number of jump clones ({max_clones}) reached for current Infomorph Psychology skill levels"
            ),
        });
    }
    Ok(PyRep::Bool(true))
}

/// `GetShipCloneState`: returns clones installed in a ship's Clone Vat Bay (`jumpCloneID`, `ownerID`, `locationID`, `cloneName`).
pub fn get_ship_clone_state(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let host_ship_id = args
        .first()
        .and_then(|a| a.as_i64())
        .or(session.ship_id)
        .unwrap_or(0);
    let rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
        snap.jump_clones
            .values()
            .flat_map(|v| v.iter())
            .filter(|jc| jc.location_id == host_ship_id)
            .map(|jc| {
                vec![
                    PyRep::Integer(jc.jump_clone_id),
                    PyRep::Integer(jc.character_id),
                    PyRep::Integer(jc.location_id),
                    PyRep::String(name_of(snap, jc.jump_clone_id)),
                ]
            })
            .collect()
    });
    Ok(PyRep::rowset(
        vec!["jumpCloneID", "ownerID", "locationID", "cloneName"],
        rows,
    ))
}

