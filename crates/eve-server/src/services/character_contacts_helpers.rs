//! Watchlist presence notifications, contact ID list parsing, and home-station helpers.

#![forbid(unsafe_code)]

use super::is_blocked_by;
use crate::router::{NotificationTarget, ServerContext};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

pub fn parse_id_list(arg: Option<&PyRep>) -> Vec<i64> {
    match arg {
        Some(PyRep::List(items) | PyRep::Tuple(items)) => {
            items.iter().filter_map(|v| v.as_i64()).collect()
        }
        Some(v) => v.as_i64().into_iter().collect(),
        None => Vec::new(),
    }
}

pub fn is_character_online(ctx: &ServerContext, target_char_id: i64) -> bool {
    let uid_opt = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&target_char_id).map(|c| c.user_id)
    });
    uid_opt.is_some_and(|uid| {
        ctx.db
            .get_online_session(uid)
            .is_some_and(|os| os.character_id == Some(target_char_id))
    })
}

/// Emits `OnClientContactChange(charID, online)` to all online characters who have `logged_on_char_id`
/// on their watchlist and are not blocked by it (`feat:social.contacts-buddy-online`, `feat:social.contacts-block`).
pub fn notify_watchlist_logon(ctx: &ServerContext, logged_on_char_id: i64) {
    notify_watchlist_presence(ctx, logged_on_char_id, true);
}

/// See [`notify_watchlist_logon`]; `online == false` is sent when the watched character logs out.
pub fn notify_watchlist_presence(ctx: &ServerContext, logged_on_char_id: i64, online: bool) {
    let watchers: Vec<i64> = ctx
        .esi_state
        .char_contacts
        .read()
        .ok()
        .map(|g| {
            let blocked_by_target = |watcher_id: i64| -> bool {
                g.get(&logged_on_char_id)
                    .and_then(|list| {
                        list.iter()
                            .find(|e| e.contact_id == watcher_id)
                            .map(|e| e.is_blocked)
                    })
                    .unwrap_or(false)
            };
            g.iter()
                .filter_map(|(&watcher_id, contacts)| {
                    if watcher_id != logged_on_char_id
                        && contacts
                            .iter()
                            .any(|c| c.contact_id == logged_on_char_id && c.is_watched)
                        && !blocked_by_target(watcher_id)
                    {
                        Some(watcher_id)
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    for watcher_id in watchers {
        ctx.notify(
            NotificationTarget::Character(watcher_id),
            "OnClientContactChange",
            "charid",
            vec![PyRep::Integer(logged_on_char_id), PyRep::Bool(online)],
        );
    }
}

pub(crate) fn handle_home_station_candidates(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let ch = ctx.db.get_character(char_id).map_err(|e| BridgeRpcError {
        code: "CharacterNotFound".into(),
        message: e.to_string(),
    })?;
    let mut cands = vec![ch.home_station_id];
    let school_st = ctx
        .sde
        .schools
        .get(&ch.school_id)
        .map(|s| s.station_id)
        .filter(|&id| id > 0);
    let hq_st = ctx.db.with_snapshot(|snap| {
        snap.corporations
            .get(&ch.corporation_id)
            .and_then(|c| c.hq_station_id)
            .filter(|&id| id > 0)
    });
    for st in [school_st, hq_st, session.station_id, session.structure_id]
        .into_iter()
        .flatten()
    {
        if !cands.contains(&st) {
            cands.push(st);
        }
    }
    Ok(PyRep::List(cands.into_iter().map(PyRep::Integer).collect()))
}

pub(crate) fn handle_self_destruct_clone(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_none() && session.structure_id.is_none() {
        return Err(BridgeRpcError::user_error("CannotSelfDestructInSpace"));
    }
    let ship_id = session.ship_id.unwrap_or(0);
    let is_capsule = ctx.db.with_snapshot(|snap| {
        snap.items
            .get(&ship_id)
            .map(|i| {
                i.type_id == 670
                    || i.type_id == 33328
                    || ctx
                        .sde
                        .types
                        .get(&i.type_id)
                        .map(|t| t.group_id == 29)
                        .unwrap_or(false)
            })
            .unwrap_or(false)
    });
    if !is_capsule {
        return Err(BridgeRpcError::user_error("SelfDestructOnlyInCapsule"));
    }
    crate::services::station_structure::structure_lifecycle::prepare_home_station_respawn(
        ctx, char_id,
    );
    match ctx.db.respawn_character_at_home_station(char_id, ship_id, &ctx.sde) {
        Ok((ch, capsule)) => {
            session.station_id = ch.station_id;
            session.structure_id = ch.structure_id;
            session.solar_system_id = Some(ch.solar_system_id);
            session.constellation_id = Some(ch.constellation_id);
            session.region_id = Some(ch.region_id);
            session.ship_id = Some(capsule.item_id);
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnCloneDestruction",
                "charid",
                vec![],
            );
            Ok(PyRep::Integer(ch.home_station_id))
        }
        Err(e) => Err(BridgeRpcError {
            code: "RespawnFailed".into(),
            message: e.to_string(),
        }),
    }
}

pub(crate) fn check_mail_recipient_blocks(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<(), BridgeRpcError> {
    if let Some(sender_id) = session.character_id {
        for rec_id in parse_id_list(args.first()) {
            if is_blocked_by(ctx, rec_id, sender_id) {
                return Err(BridgeRpcError {
                    code: "ContactBlocked".into(),
                    message: format!("Recipient {rec_id} has blocked sender {sender_id}"),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "character_contacts_helpers_cov_unit_tests.rs"]
mod character_contacts_helpers_cov_unit_tests;

