//! `character_lifecycle` — character slot accounting and biomassing (`charUnboundMgr`).
//!
//! Implements `GetNumCharacters`, `PrepareCharacterForDelete`, `CancelCharacterDeletePrepare` and
//! `DeleteCharacter`, and guards `SelectCharacterID` / `CreateCharacterWithDoll` (the main
//! handlers in `character_gm_tools` run afterwards when the guard passes).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Private JSON side-store key in `item_custom_names`.
pub(crate) const CHARACTER_LIFECYCLE_STORE_KEY: i64 = -10_000_005;

/// Character slots per account (V24: three characters per account).
pub(crate) const MAX_CHARACTERS_PER_ACCOUNT: usize = 3;

/// DERIVED: delay between `PrepareCharacterForDelete` and an allowed `DeleteCharacter`.
/// The live value is not present in the client or SDE; 10 hours is an assumption.
pub(crate) const BIOMASS_DELAY_SECS: u64 = 10 * 3_600;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct LifecycleStore {
    /// characterID -> unix time the delete was prepared.
    #[serde(default)]
    delete_prepared_unix: HashMap<i64, u64>,
}

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

fn load_store(snap: &eve_db::WorldStateSnapshot) -> LifecycleStore {
    snap.item_custom_names
        .get(&CHARACTER_LIFECYCLE_STORE_KEY)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

fn save_store(snap: &mut eve_db::WorldStateSnapshot, store: &LifecycleStore) {
    if let Ok(json) = serde_json::to_string(store) {
        snap.item_custom_names
            .insert(CHARACTER_LIFECYCLE_STORE_KEY, json);
    }
}

/// Doomheim is resolved from the SDE NPC corporations by name (no hardcoded corporationID).
fn doomheim_corp_id(ctx: &ServerContext) -> Option<i64> {
    ctx.sde
        .npc_corporations
        .values()
        .find(|c| c.name == "Doomheim")
        .map(|c| c.corporation_id)
}

fn owned_char_arg(
    ctx: &ServerContext,
    user_id: u64,
    args: &[PyRep],
    method: &str,
) -> Result<i64, BridgeRpcError> {
    let char_id = args
        .first()
        .and_then(|a| a.as_i64())
        .ok_or_else(|| err("InvalidArgument", format!("{method} requires characterID")))?;
    let owns = ctx
        .db
        .get_characters_for_user(user_id)
        .iter()
        .any(|c| c.character_id == char_id);
    if !owns {
        return Err(err(
            "CharacterOwnershipMismatch",
            format!("Character {char_id} does not belong to this account"),
        ));
    }
    Ok(char_id)
}

/// Returns `Some(result)` for RPCs fully handled here; `None` lets the regular handler run.
pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "charUnboundMgr" {
        return None;
    }
    let user_id = match ServerContext::require_auth(session) {
        Ok(u) => u,
        Err(e) => return Some(Err(e)),
    };
    match method {
        "GetNumCharacters" => {
            let n = ctx.db.get_characters_for_user(user_id).len();
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Integer(n as i64),
                PyRep::Integer(MAX_CHARACTERS_PER_ACCOUNT as i64),
            ])))
        }
        "CreateCharacterWithDoll" => {
            if ctx.db.get_characters_for_user(user_id).len() >= MAX_CHARACTERS_PER_ACCOUNT {
                return Some(Err(err(
                    "MaxCharacterSlotsReached",
                    format!("An account can hold at most {MAX_CHARACTERS_PER_ACCOUNT} characters"),
                )));
            }
            None
        }
        "SelectCharacterID" => {
            let char_id = args.first().and_then(|a| a.as_i64())?;
            let pending = ctx
                .db
                .with_snapshot(|s| load_store(s).delete_prepared_unix.contains_key(&char_id));
            if pending {
                return Some(Err(err(
                    "CharacterLocked",
                    format!("Character {char_id} is pending deletion"),
                )));
            }
            None
        }
        "PrepareCharacterForDelete" => Some(prepare(ctx, user_id, args)),
        "CancelCharacterDeletePrepare" => Some(cancel(ctx, user_id, args)),
        "DeleteCharacter" => Some(delete(ctx, user_id, args)),
        _ => None,
    }
}

fn prepare(ctx: &ServerContext, user_id: u64, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let char_id = owned_char_arg(ctx, user_id, args, "PrepareCharacterForDelete")?;
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        store.delete_prepared_unix.insert(char_id, now);
        save_store(snap, &store);
    });
    Ok(PyRep::Integer((now + BIOMASS_DELAY_SECS) as i64))
}

fn cancel(ctx: &ServerContext, user_id: u64, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let char_id = owned_char_arg(ctx, user_id, args, "CancelCharacterDeletePrepare")?;
    let removed = ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let removed = store.delete_prepared_unix.remove(&char_id).is_some();
        save_store(snap, &store);
        removed
    });
    Ok(PyRep::Bool(removed))
}

fn delete(ctx: &ServerContext, user_id: u64, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let char_id = owned_char_arg(ctx, user_id, args, "DeleteCharacter")?;
    let doomheim = doomheim_corp_id(ctx)
        .ok_or_else(|| err("SdeMissing", "Doomheim corporation not found in SDE"))?;
    let now = ctx.now_unix();
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let Some(&prepared) = store.delete_prepared_unix.get(&char_id) else {
            return Err(err(
                "DeleteNotPrepared",
                "PrepareCharacterForDelete must be called first",
            ));
        };
        if now < prepared + BIOMASS_DELAY_SECS {
            return Err(err(
                "DeleteTimerNotExpired",
                format!(
                    "Character can be deleted after {}",
                    prepared + BIOMASS_DELAY_SECS
                ),
            ));
        }
        store.delete_prepared_unix.remove(&char_id);
        save_store(snap, &store);
        let ch = snap.characters.get_mut(&char_id).ok_or_else(|| {
            err(
                "CharacterNotFound",
                format!("Character {char_id} not found"),
            )
        })?;
        // Biomassed: the character leaves the account (freeing the slot) and joins Doomheim.
        ch.user_id = 0;
        ch.corporation_id = doomheim;
        ch.alliance_id = None;
        ch.war_faction_id = None;
        ch.version += 1;
        Ok(())
    })?;
    Ok(PyRep::keyval([
        ("characterID", PyRep::Integer(char_id)),
        ("corporationID", PyRep::Integer(doomheim)),
        ("deleted", PyRep::Bool(true)),
    ]))
}
