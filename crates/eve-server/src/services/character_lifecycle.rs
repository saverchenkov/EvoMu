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
pub struct LifecycleStore {
    /// characterID -> unix time the delete was prepared.
    #[serde(default)]
    pub delete_prepared_unix: HashMap<i64, u64>,
}

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

pub fn load_store(snap: &eve_db::WorldStateSnapshot) -> LifecycleStore {
    snap.item_custom_names
        .get(&CHARACTER_LIFECYCLE_STORE_KEY)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

pub fn save_store(snap: &mut eve_db::WorldStateSnapshot, store: &LifecycleStore) {
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

fn unix_to_filetime(unix_secs: u64) -> i64 {
    (unix_secs as i64 + 11_644_473_600) * 10_000_000
}

/// Returns `Some(result)` for RPCs fully handled here; `None` lets the regular handler run.
pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service == "charMgr" {
        return handle_char_mgr(ctx, session, method, args);
    }
    if service != "charUnboundMgr" {
        return None;
    }
    let user_id = match ServerContext::require_auth(session) {
        Ok(u) => u,
        Err(e) => return Some(Err(e)),
    };
    match (service, method) {
        ("charUnboundMgr", "GetNumCharacters") => {
            let n = ctx.db.get_characters_for_user(user_id).len();
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Integer(n as i64),
                PyRep::Integer(MAX_CHARACTERS_PER_ACCOUNT as i64),
            ])))
        }
        ("charUnboundMgr", "CreateCharacterWithDoll") => {
            if ctx.db.get_characters_for_user(user_id).len() >= MAX_CHARACTERS_PER_ACCOUNT {
                return Some(Err(err(
                    "MaxCharacterSlotsReached",
                    format!("An account can hold at most {MAX_CHARACTERS_PER_ACCOUNT} characters"),
                )));
            }
            None
        }
        ("charUnboundMgr", "SelectCharacterID") => {
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
        ("charUnboundMgr", "PrepareCharacterForDelete") => Some(prepare(ctx, user_id, args)),
        ("charUnboundMgr", "CancelCharacterDeletePrepare") => Some(cancel(ctx, user_id, args)),
        ("charUnboundMgr", "DeleteCharacter") => Some(delete(ctx, session, user_id, args)),
        ("charUnboundMgr", "GetQAStarterSystemIDs") => {
            Some(Ok(PyRep::List(vec![
                PyRep::Integer(30000142),
                PyRep::Integer(30000144),
                PyRep::Integer(30002187),
                PyRep::Integer(30002510),
            ])))
        }
        ("charUnboundMgr", "ToggleValidation") => {
            // const-ok: toggle validation confirmation for character creation pipeline
            Some(Ok(PyRep::Bool(true)))
        }
        ("charUnboundMgr", "UpdateCharacterBloodline") => {
            let char_id = match owned_char_arg(ctx, user_id, args, "UpdateCharacterBloodline") {
                Ok(id) => id,
                Err(e) => return Some(Err(e)),
            };
            let bloodline_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
            let updated = ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.bloodline_id = bloodline_id as u32;
                    ch.version += 1;
                    true
                } else {
                    false
                }
            });
            Some(Ok(PyRep::Bool(updated)))
        }
        ("charUnboundMgr", "UpdateCharacterGender") => {
            let char_id = match owned_char_arg(ctx, user_id, args, "UpdateCharacterGender") {
                Ok(id) => id,
                Err(e) => return Some(Err(e)),
            };
            let gender_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let updated = ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.gender = gender_id as u32;
                    ch.version += 1;
                    true
                } else {
                    false
                }
            });
            Some(Ok(PyRep::Bool(updated)))
        }
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

fn delete(ctx: &ServerContext, session: &SessionState, user_id: u64, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let char_id = owned_char_arg(ctx, user_id, args, "DeleteCharacter")?;
    let doomheim = doomheim_corp_id(ctx)
        .ok_or_else(|| err("SdeMissing", "Doomheim corporation not found in SDE"))?;
    let now = ctx.now_unix();
    let is_forced = session.has_gm_role();
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let prepared = match store.delete_prepared_unix.get(&char_id).copied() {
            Some(t) => t,
            None => {
                if !is_forced {
                    return Err(err(
                        "DeleteNotPrepared",
                        "PrepareCharacterForDelete must be called first",
                    ));
                }
                now
            }
        };
        if !is_forced && now < prepared + BIOMASS_DELAY_SECS {
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
        ("deleted", PyRep::Bool(true)),
        ("characterID", PyRep::Integer(char_id)),
    ]))
}

fn handle_char_mgr(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match method {
        "GetCharacterCreationDate" => {
            let (_, session_char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let char_id = args.first().and_then(|a| a.as_i64()).unwrap_or(session_char_id);
            let dt = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| unix_to_filetime(c.create_date_unix))
                    .unwrap_or_else(|| ctx.now_blueos_filetime_ticks())
            });
            Some(Ok(PyRep::Integer(dt)))
        }
        "GetHomeStationRow" => {
            let (_, session_char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let char_id = args.first().and_then(|a| a.as_i64()).unwrap_or(session_char_id);
            let station_id = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| c.home_station_id)
                    .unwrap_or(60_003_760)
            });
            Some(Ok(PyRep::keyval([
                ("stationID", PyRep::Integer(station_id)),
                ("typeID", PyRep::Integer(1531)),
            ])))
        }
        "stationID" => {
            let (_, session_char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let st_id = session.station_id.unwrap_or_else(|| {
                ctx.db.with_snapshot(|snap| {
                    snap.characters
                        .get(&session_char_id)
                        .and_then(|c| c.station_id.or(Some(c.home_station_id)))
                        .unwrap_or(60_003_760)
                })
            });
            Some(Ok(PyRep::Integer(st_id)))
        }
        "GetOrganizationInfoForCharacters" => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let char_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(list)) | Some(PyRep::Tuple(list)) => {
                    list.iter().filter_map(|v| v.as_i64()).collect()
                }
                Some(PyRep::Integer(id)) => vec![*id],
                _ => Vec::new(),
            };
            let res = ctx.db.with_snapshot(|snap| {
                char_ids
                    .into_iter()
                    .map(|cid| {
                        let ch = snap.characters.get(&cid);
                        let corp = ch.map(|c| c.corporation_id).unwrap_or(1000044);
                        let ally = ch.and_then(|c| c.alliance_id);
                        (
                            PyRep::Integer(cid),
                            PyRep::keyval([
                                ("characterID", PyRep::Integer(cid)),
                                ("corporationID", PyRep::Integer(corp)),
                                ("allianceID", ally.map(PyRep::Integer).unwrap_or(PyRep::None)),
                            ]),
                        )
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::Dict(res)))
        }
        "GetPrivateInfoOnCorpChange" => {
            let (_, session_char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let char_id = args.first().and_then(|a| a.as_i64()).unwrap_or(session_char_id);
            let corp_id = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(1000044)
            });
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                ("corporationID", PyRep::Integer(corp_id)),
                ("roles", PyRep::Integer(0)),
            ])))
        }
        "GetPublicInfo" => {
            let (_, session_char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let char_id = args.first().and_then(|a| a.as_i64()).unwrap_or(session_char_id);
            let info = ctx.db.with_snapshot(|snap| {
                snap.characters.get(&char_id).map(|c| {
                    (
                        c.name.clone(),
                        c.corporation_id,
                        c.alliance_id,
                        c.security_status,
                        c.description.clone(),
                        unix_to_filetime(c.create_date_unix),
                    )
                })
            });
            let (name, corp, ally, sec, desc, cdate) = info.unwrap_or((
                "Unknown".into(),
                1000044,
                None,
                0.0,
                String::new(),
                ctx.now_blueos_filetime_ticks(),
            ));
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                ("characterName", PyRep::String(name)),
                ("corporationID", PyRep::Integer(corp)),
                ("allianceID", ally.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("securityStatus", PyRep::Real(sec)),
                ("description", PyRep::String(desc)),
                ("createDateTime", PyRep::Integer(cdate)),
            ])))
        }
        "GetRecentShipKillsAndLosses" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.killmails
                    .values()
                    .filter(|km| km.victim_character_id == char_id || km.attacker_character_id == char_id)
                    .map(|km| vec![
                        PyRep::Integer(km.kill_id),
                        PyRep::Integer(km.victim_character_id),
                        PyRep::Integer(km.solar_system_id as i64),
                        PyRep::Integer(km.killed_unix as i64),
                    ])
                    .collect()
            });
            Some(Ok(PyRep::Rowset {
                columns: vec!["killID".into(), "victimID".into(), "solarSystemID".into(), "killTime".into()],
                rows,
            }))
        }
        "SetActivityStatus" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let status = args.first().and_then(|a| a.as_str()).unwrap_or("active");
            ctx.db.with_mut_snapshot(|snap| {
                snap.item_custom_names.insert(
                    -(char_id & 0x7FFF_FFFF),
                    format!("activityStatus:{status}"),
                );
            });
            Some(Ok(PyRep::Bool(true)))
        }
        "GetCohortsForCharacter" => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::List(Vec::new())))
        }
        "SetSessionCheck" | "clear_memoized" => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::Bool(true)))
        }
        _ => None,
    }
}
