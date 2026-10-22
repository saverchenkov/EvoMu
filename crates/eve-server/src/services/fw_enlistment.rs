//! `fw_enlistment` — Faction Warfare / Insurgency militia enlistment (`fwCharacterEnlistmentMgr`).
//!
//! Implements the V24 client surface used by `eve/client/script/ui/services/fwEnlistmentSvc.pyj`:
//! `GetMyEnlistment`, `CreateMyDirectEnlistment`, `RemoveMyDirectEnlistment`,
//! `GetMyDirectEnlistmentCooldownTimestamp`, `GetCorpAllowedEnlistmentFactions` and
//! `SetMyCorpAllowedEnlistmentFactions`.
//!
//! Militia factions are derived from the SDE (`sde_factions.militia_corporation_id IS NOT NULL`);
//! there is no hardcoded faction table here.

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_db::corp_roles;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Private JSON side-store key in `item_custom_names` (same pattern as the account/char stores).
pub(crate) const FW_ENLISTMENT_STORE_KEY: i64 = -10_000_003;

/// DERIVED: cooldown applied after leaving a militia via `RemoveMyDirectEnlistment`.
/// The exact live-server value is not present in the client or SDE; 24 hours is an assumption
/// documented here so it can be replaced once a primary source is available.
pub(crate) const DIRECT_ENLISTMENT_COOLDOWN_SECS: u64 = 24 * 3_600;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct FwEnlistmentStore {
    /// characterID -> unix time until which direct enlistment is blocked.
    #[serde(default)]
    pub cooldown_until: HashMap<i64, u64>,
    /// corporationID -> allowed militia factionIDs set by a director (player corps only).
    #[serde(default)]
    pub corp_allowed: HashMap<i64, Vec<i64>>,
}

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: message.into() }
}

/// Unix seconds -> Windows FILETIME ticks (100ns since 1601), the blue time format the client reads.
pub(crate) fn unix_to_filetime(unix_secs: u64) -> i64 {
    (unix_secs as i64 + 11_644_473_600).saturating_mul(10_000_000)
}

fn load_store(snap: &eve_db::WorldStateSnapshot) -> FwEnlistmentStore {
    snap.item_custom_names
        .get(&FW_ENLISTMENT_STORE_KEY)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

fn save_store(snap: &mut eve_db::WorldStateSnapshot, store: &FwEnlistmentStore) {
    if let Ok(json) = serde_json::to_string(store) {
        snap.item_custom_names.insert(FW_ENLISTMENT_STORE_KEY, json);
    }
}

/// Militia factions derived from the SDE: every faction that has a militia corporation.
pub(crate) fn is_militia_faction(ctx: &ServerContext, faction_id: i64) -> bool {
    u32::try_from(faction_id)
        .ok()
        .and_then(|fid| ctx.sde.factions.get(&fid))
        .is_some_and(|f| f.militia_corporation_id.is_some())
}

fn enlistment_tuple(faction: Option<i64>) -> PyRep {
    // (factionID, directFactionID, corpFactionID) — only direct enlistment is modelled here.
    let f = faction.map(PyRep::Integer).unwrap_or(PyRep::None);
    PyRep::Tuple(vec![f.clone(), f, PyRep::None])
}

fn notify_state(ctx: &ServerContext, char_id: i64, faction: Option<i64>) {
    let f = faction.map(PyRep::Integer).unwrap_or(PyRep::None);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnEnlistmentStateUpdated",
        "charid",
        vec![f.clone(), f, PyRep::None],
    );
}

fn faction_list_arg(ctx: &ServerContext, arg: Option<&PyRep>) -> Result<Vec<i64>, BridgeRpcError> {
    let items: Vec<PyRep> = match arg {
        Some(PyRep::List(v)) | Some(PyRep::Tuple(v)) => v.clone(),
        Some(PyRep::None) | None => Vec::new(),
        Some(other) => return Err(err("InvalidFactionList", format!("Expected list of factionIDs, got {other:?}"))),
    };
    let mut out = Vec::new();
    for it in items {
        let fid = it.as_i64().ok_or_else(|| err("InvalidFactionList", "factionIDs must be integers"))?;
        if !is_militia_faction(ctx, fid) {
            return Err(err("InvalidWarFaction", format!("Faction {fid} has no militia")));
        }
        if !out.contains(&fid) {
            out.push(fid);
        }
    }
    out.sort_unstable();
    Ok(out)
}

/// Returns `Some(result)` when `(service, method)` belongs to `fwCharacterEnlistmentMgr`.
pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "fwCharacterEnlistmentMgr" {
        return None;
    }
    let res = match method {
        "GetMyEnlistment" => get_my_enlistment(ctx, session),
        "CreateMyDirectEnlistment" => create_direct(ctx, session, args),
        "RemoveMyDirectEnlistment" => remove_direct(ctx, session),
        "GetMyDirectEnlistmentCooldownTimestamp" => cooldown_timestamp(ctx, session),
        "GetCorpAllowedEnlistmentFactions" => get_corp_allowed(ctx, session, args),
        "SetMyCorpAllowedEnlistmentFactions" => set_corp_allowed(ctx, session, args),
        _ => return None,
    };
    Some(res)
}

fn get_my_enlistment(ctx: &ServerContext, session: &SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let faction = ctx.db.with_snapshot(|s| s.characters.get(&char_id).and_then(|c| c.war_faction_id));
    Ok(enlistment_tuple(faction))
}

fn create_direct(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let faction_id = args
        .first()
        .and_then(|a| a.as_i64())
        .ok_or_else(|| err("InvalidWarFaction", "CreateMyDirectEnlistment requires a factionID"))?;
    if !is_militia_faction(ctx, faction_id) {
        return Err(err("InvalidWarFaction", format!("Faction {faction_id} has no militia")));
    }
    let now = ctx.now_unix();
    ctx.db.try_with_mut_snapshot(|snap| {
        let store = load_store(snap);
        let ch = snap
            .characters
            .get(&char_id)
            .ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;
        if let Some(current) = ch.war_faction_id {
            return Err(err("FwAlreadyEnlisted", format!("Character is already enlisted with faction {current}")));
        }
        if let Some(&until) = store.cooldown_until.get(&char_id) {
            if now < until {
                return Err(err("FwEnlistmentCooldown", format!("Direct enlistment is on cooldown until {until}")));
            }
        }
        let standing = ch.standings.get(&faction_id).copied().unwrap_or(0.0);
        if standing < 0.0 {
            return Err(err(
                "FwStandingTooLow",
                format!("Standing {standing:.2} towards faction {faction_id} must be non-negative"),
            ));
        }
        // Player corporations may restrict which militias their members can join.
        if snap.corporations.contains_key(&ch.corporation_id) {
            if let Some(allowed) = store.corp_allowed.get(&ch.corporation_id) {
                if !allowed.contains(&faction_id) {
                    return Err(err(
                        "FwFactionNotAllowedByCorp",
                        format!("Corporation {} does not allow enlistment with faction {faction_id}", ch.corporation_id),
                    ));
                }
            }
        }
        let ch = snap.characters.get_mut(&char_id).expect("checked above");
        ch.war_faction_id = Some(faction_id);
        ch.version += 1;
        Ok(())
    })?;
    session.war_faction_id = Some(faction_id);
    notify_state(ctx, char_id, Some(faction_id));
    Ok(PyRep::None)
}

fn remove_direct(ctx: &ServerContext, session: &mut SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let until = ctx.now_unix() + DIRECT_ENLISTMENT_COOLDOWN_SECS;
    ctx.db.try_with_mut_snapshot(|snap| {
        let ch = snap
            .characters
            .get_mut(&char_id)
            .ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;
        if ch.war_faction_id.is_none() {
            return Err(err("FwNotEnlisted", "Character is not directly enlisted"));
        }
        ch.war_faction_id = None;
        ch.version += 1;
        let mut store = load_store(snap);
        store.cooldown_until.insert(char_id, until);
        save_store(snap, &store);
        Ok(())
    })?;
    session.war_faction_id = None;
    notify_state(ctx, char_id, None);
    Ok(PyRep::None)
}

fn cooldown_timestamp(ctx: &ServerContext, session: &SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now = ctx.now_unix();
    let mut expired = false;
    let until = ctx.db.with_mut_snapshot(|s| {
        let mut store = load_store(s);
        let u = store.cooldown_until.get(&char_id).copied();
        if let Some(ts) = u {
            if ts <= now {
                store.cooldown_until.remove(&char_id);
                save_store(s, &store);
                expired = true;
            }
        }
        u
    });
    if expired {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnEnlistmentCooldownCleared",
            "charid",
            vec![],
        );
    }
    Ok(match until {
        Some(u) if u > now => PyRep::Integer(unix_to_filetime(u)),
        _ => PyRep::None,
    })
}

fn get_corp_allowed(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let corp_id = args
        .first()
        .and_then(|a| a.as_i64())
        .or(session.corporation_id)
        .ok_or_else(|| err("CorporationNotFound", "GetCorpAllowedEnlistmentFactions requires a corporationID"))?;
    let allowed = ctx.db.with_snapshot(|s| load_store(s).corp_allowed.get(&corp_id).cloned());
    Ok(match allowed {
        Some(list) => PyRep::List(list.into_iter().map(PyRep::Integer).collect()),
        None => PyRep::None,
    })
}

fn set_corp_allowed(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let factions = faction_list_arg(ctx, args.first())?;
    let corp_id = ctx.db.try_with_mut_snapshot(|snap| {
        let corp_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;
        let corp = snap
            .corporations
            .get(&corp_id)
            .ok_or_else(|| err("CorpRoleDenied", "NPC corporations cannot configure militia enlistment"))?;
        let role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
        if char_id != corp.ceo_id && !corp_roles::has_role(role, corp_roles::ROLE_DIRECTOR) {
            return Err(err("CorpRoleDenied", "Only the CEO or a director may set allowed enlistment factions"));
        }
        let mut store = load_store(snap);
        store.corp_allowed.insert(corp_id, factions.clone());
        save_store(snap, &store);
        Ok(corp_id)
    })?;
    ctx.notify(
        NotificationTarget::Corporation(corp_id),
        "OnCorpAllowedEnlistmentFactionsSet",
        "corpid",
        vec![PyRep::List(factions.iter().copied().map(PyRep::Integer).collect())],
    );
    Ok(PyRep::List(factions.into_iter().map(PyRep::Integer).collect()))
}
