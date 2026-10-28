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

pub(crate) fn load_store(snap: &eve_db::WorldStateSnapshot) -> FwEnlistmentStore {
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

fn enlistment_tuple_full(
    effective: Option<i64>,
    direct: Option<i64>,
    corp: Option<i64>,
) -> PyRep {
    let e = effective.map(PyRep::Integer).unwrap_or(PyRep::None);
    let d = direct.map(PyRep::Integer).unwrap_or(PyRep::None);
    let c = corp.map(PyRep::Integer).unwrap_or(PyRep::None);
    PyRep::Tuple(vec![e, d, c])
}

fn notify_state(
    ctx: &ServerContext,
    char_id: i64,
    effective: Option<i64>,
    direct: Option<i64>,
    corp: Option<i64>,
) {
    let e = effective.map(PyRep::Integer).unwrap_or(PyRep::None);
    let d = direct.map(PyRep::Integer).unwrap_or(PyRep::None);
    let c = corp.map(PyRep::Integer).unwrap_or(PyRep::None);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnEnlistmentStateUpdated",
        "charid",
        vec![e, d, c],
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
    let (eff, direct, corp) = ctx.db.with_snapshot(|s| {
        let ch = s.characters.get(&char_id);
        let corp_fac = ch.and_then(|c| s.fw_runtime.corp_war_factions.get(&c.corporation_id).copied());
        let direct_fac = s.fw_runtime.direct_enlisted_chars.get(&char_id).copied();
        let eff_fac = ch.and_then(|c| c.war_faction_id).or(direct_fac).or(corp_fac);
        let resolved_direct = if direct_fac.is_none() && corp_fac.is_none() {
            eff_fac
        } else {
            direct_fac
        };
        (eff_fac, resolved_direct, corp_fac)
    });
    Ok(enlistment_tuple_full(eff, direct, corp))
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
    let militia_corp_opt = u32::try_from(faction_id)
        .ok()
        .and_then(|fid| ctx.sde.factions.get(&fid))
        .and_then(|f| f.militia_corporation_id)
        .map(i64::from);
    let moved_corp_opt = ctx.db.try_with_mut_snapshot(|snap| {
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
        let old_corp_id = ch.corporation_id;
        let in_player_corp = snap.corporations.contains_key(&old_corp_id);
        // Player corporations may restrict which militias their members can join.
        if in_player_corp {
            if let Some(allowed) = store.corp_allowed.get(&old_corp_id) {
                if !allowed.contains(&faction_id) {
                    return Err(err(
                        "FwFactionNotAllowedByCorp",
                        format!("Corporation {} does not allow enlistment with faction {faction_id}", old_corp_id),
                    ));
                }
            }
        }
        let mut new_corp_opt = None;
        let ch = snap.characters.get_mut(&char_id).expect("checked above");
        ch.war_faction_id = Some(faction_id);
        if !in_player_corp {
            if let Some(militia_corp_id) = militia_corp_opt {
                if militia_corp_id != old_corp_id {
                    ch.corporation_id = militia_corp_id;
                    new_corp_opt = Some((old_corp_id, militia_corp_id));
                }
            }
        }
        ch.version += 1;
        if let Some((old_cid, new_cid)) = new_corp_opt {
            crate::services::fleet_social::fleet_social_corps::record_corp_join_in_snap_pub(
                snap,
                char_id,
                old_cid,
                new_cid,
                now,
            );
        }
        let rec = snap.fw_character_stats.entry(char_id).or_insert_with(|| {
            eve_db::snapshot_ext::FwCharacterStatsRecord {
                character_id: char_id,
                faction_id: Some(faction_id),
                enlisted_on_unix: Some(now),
                current_rank: 1,
                highest_rank: 1,
                kills: Default::default(),
                victory_points: Default::default(),
            }
        });
        rec.faction_id = Some(faction_id);
        rec.enlisted_on_unix = Some(now);
        snap.fw_runtime.direct_enlisted_chars.insert(char_id, faction_id);
        Ok(new_corp_opt)
    })?;
    if let Some((_, new_cid)) = moved_corp_opt {
        session.corporation_id = Some(new_cid);
    }
    session.war_faction_id = Some(faction_id);
    let corp_fac = ctx.db.with_snapshot(|s| {
        s.characters
            .get(&char_id)
            .and_then(|c| s.fw_runtime.corp_war_factions.get(&c.corporation_id).copied())
    });
    notify_state(ctx, char_id, Some(faction_id), Some(faction_id), corp_fac);
    Ok(PyRep::None)
}

fn remove_direct(ctx: &ServerContext, session: &mut SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now = ctx.now_unix();
    let until = now + DIRECT_ENLISTMENT_COOLDOWN_SECS;
    let (moved_corp_opt, inherited_corp_fac) = ctx.db.try_with_mut_snapshot(|snap| {
        let old_corp_id = snap
            .characters
            .get(&char_id)
            .ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?
            .corporation_id;
        let in_player_corp = snap.corporations.contains_key(&old_corp_id);
        let default_npc_corp = if !in_player_corp {
            Some(crate::services::fleet_social::fleet_social_corp_tail::bloodline_default_npc_corp(
                ctx, snap, char_id,
            ))
        } else {
            None
        };
        let corp_fac = snap.fw_runtime.corp_war_factions.get(&old_corp_id).copied();
        let ch = snap.characters.get_mut(&char_id).expect("checked above");
        if ch.war_faction_id.is_none() {
            return Err(err("FwNotEnlisted", "Character is not directly enlisted"));
        }
        ch.war_faction_id = corp_fac;
        let mut new_corp_opt = None;
        if let Some(def_corp) = default_npc_corp {
            if def_corp != old_corp_id {
                ch.corporation_id = def_corp;
                new_corp_opt = Some((old_corp_id, def_corp));
            }
        }
        ch.version += 1;
        snap.fw_runtime.direct_enlisted_chars.remove(&char_id);
        if let Some((old_cid, new_cid)) = new_corp_opt {
            crate::services::fleet_social::fleet_social_corps::record_corp_join_in_snap_pub(
                snap,
                char_id,
                old_cid,
                new_cid,
                now,
            );
        }
        let mut store = load_store(snap);
        store.cooldown_until.insert(char_id, until);
        save_store(snap, &store);
        Ok((new_corp_opt, corp_fac))
    })?;
    if let Some((_, new_cid)) = moved_corp_opt {
        session.corporation_id = Some(new_cid);
    }
    session.war_faction_id = inherited_corp_fac;
    notify_state(ctx, char_id, inherited_corp_fac, None, inherited_corp_fac);
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
