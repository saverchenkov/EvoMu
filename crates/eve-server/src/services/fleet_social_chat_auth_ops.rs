//! Channel ownership, membership authorization, dynamic org operator checks (`#521`),
//! global mute lookup (`#481`), and `charMgr` contacts/CSPA/notes handlers extracted
//! from `fleet_social_chat.rs`.

use crate::router::ServerContext;
use eve_db::{corp_roles, ChatChannelRecord, FleetRole, IskCents, WorldStateSnapshot};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

pub(super) const CORP_ROLE_CHAT_MANAGER: u64 = 1 << 38;

pub(super) fn is_system_channel(chan_id: &str) -> bool {
    let lower = chan_id.to_ascii_lowercase();
    lower.starts_with("solarsystemid2:")
        || lower.starts_with("constellationid:")
        || lower.starts_with("regionid:")
        || lower.starts_with("resourcewars:")
        || lower.starts_with("incursion:")
        || lower.starts_with("factionid:")
        || lower.starts_with("help_")
        || matches!(lower.as_str(), "rookie_help" | "global" | "local")
}

pub(super) fn is_org_channel(chan_id: &str) -> bool {
    let lower = chan_id.to_ascii_lowercase();
    lower.starts_with("corpid:")
        || lower.starts_with("allianceid:")
        || lower.starts_with("fleetid:")
}

pub(super) fn is_system_or_org_channel(chan_id: &str) -> bool {
    is_system_channel(chan_id) || is_org_channel(chan_id)
}

pub(super) fn build_initial_channel_record(
    chan_id: &str,
    creator_char_id: i64,
    motd_arg: &str,
    password_arg: Option<&str>,
    is_delayed: bool,
    solar_system_id: Option<u32>,
) -> ChatChannelRecord {
    if is_system_or_org_channel(chan_id) {
        ChatChannelRecord {
            channel_id: chan_id.to_string(),
            name: chan_id.to_string(),
            motd: String::new(),
            owner_id: None,
            members: HashSet::new(),
            is_system_local: chan_id.starts_with("solarsystemid2:"),
            solar_system_id,
            is_private: false,
            operators: HashSet::new(),
            password: None,
            is_delayed_local: is_delayed,
        }
    } else {
        ChatChannelRecord {
            channel_id: chan_id.to_string(),
            name: chan_id.to_string(),
            motd: motd_arg.to_string(),
            owner_id: Some(creator_char_id),
            members: HashSet::new(),
            is_system_local: false,
            solar_system_id: None,
            is_private: chan_id.starts_with("private:"),
            operators: HashSet::from([creator_char_id]),
            password: password_arg.filter(|p| !p.is_empty()).map(str::to_string),
            is_delayed_local: false,
        }
    }
}

fn has_corp_chat_operator_rights(
    snap: &WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    corp_id: i64,
) -> bool {
    if (1_000_000..=1_999_999).contains(&corp_id) {
        return false;
    }
    let ch_in_corp = snap
        .characters
        .get(&char_id)
        .is_some_and(|c| c.corporation_id == corp_id)
        || session.corporation_id == Some(corp_id);
    if !ch_in_corp {
        return false;
    }
    let Some(_corp) = snap.corporations.get(&corp_id) else {
        return false;
    };
    let session_roles = if session.corporation_id == Some(corp_id) {
        session.corp_role
    } else {
        0
    };
    let (is_ceo, eff_roles) =
        snap.effective_corp_roles_at(corp_id, char_id, None, session_roles, 0);
    if is_ceo {
        return true;
    }
    let role_mask = eff_roles | session_roles;
    (role_mask & (corp_roles::ROLE_DIRECTOR | CORP_ROLE_CHAT_MANAGER)) != 0
}

pub(super) fn has_dynamic_org_operator_role(
    snap: &WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    chan_id: &str,
) -> bool {
    let lower = chan_id.to_ascii_lowercase();
    if let Some(cid) = lower.strip_prefix("corpid:").and_then(|s| s.parse::<i64>().ok()) {
        return has_corp_chat_operator_rights(snap, session, char_id, cid);
    }
    if let Some(aid) = lower.strip_prefix("allianceid:").and_then(|s| s.parse::<i64>().ok()) {
        let Some(ally) = snap.alliances.get(&aid) else {
            return false;
        };
        return has_corp_chat_operator_rights(snap, session, char_id, ally.executor_corp_id);
    }
    if let Some(fid) = lower.strip_prefix("fleetid:").and_then(|s| s.parse::<i64>().ok()) {
        let Some(fleet) = snap.fleets.get(&fid) else {
            return false;
        };
        if fleet.boss_id == char_id {
            return true;
        }
        return fleet.member_details.get(&char_id).is_some_and(|m| {
            matches!(m.role, FleetRole::FleetCommander | FleetRole::WingCommander)
        });
    }
    false
}

pub(super) fn has_channel_operator_auth(
    snap: &WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    chan_id: &str,
    chan_opt: Option<&ChatChannelRecord>,
) -> bool {
    if session.has_gm_role() {
        return true;
    }
    if is_system_channel(chan_id) {
        return false;
    }
    if is_org_channel(chan_id) {
        return has_dynamic_org_operator_role(snap, session, char_id, chan_id);
    }
    chan_opt.is_some_and(|c| c.owner_id == Some(char_id) || c.operators.contains(&char_id))
}

pub(super) fn has_channel_owner_auth(
    session: &SessionState,
    char_id: i64,
    chan_id: &str,
    chan: &ChatChannelRecord,
) -> bool {
    if is_system_or_org_channel(chan_id) {
        return false;
    }
    session.has_gm_role() || chan.owner_id == Some(char_id)
}

pub(super) fn validate_channel_join_auth(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    session: &SessionState,
    caller_id: i64,
    target_char: i64,
    chan_id: &str,
) -> Result<(), BridgeRpcError> {
    if session.has_gm_role() {
        return Ok(());
    }
    let ch = snap.characters.get(&target_char);
    let actual_sys = if target_char == caller_id {
        session
            .solar_system_id
            .or_else(|| ch.map(|c| c.solar_system_id))
            .unwrap_or(0)
    } else {
        ch.map(|c| c.solar_system_id).unwrap_or(0)
    };
    let actual_corp = if target_char == caller_id {
        session
            .corporation_id
            .or_else(|| ch.map(|c| c.corporation_id))
    } else {
        ch.map(|c| c.corporation_id)
    };
    let actual_ally = if target_char == caller_id {
        session
            .alliance_id
            .or_else(|| ch.and_then(|c| c.alliance_id))
    } else {
        ch.and_then(|c| c.alliance_id)
    };
    let actual_faction = if target_char == caller_id {
        session
            .war_faction_id
            .or_else(|| ch.and_then(|c| c.war_faction_id))
    } else {
        ch.and_then(|c| c.war_faction_id)
    };

    let deny = || BridgeRpcError {
        code: "ChannelAccessDenied".into(),
        message: format!("Not authorized to join channel {chan_id}"),
    };

    let lower = chan_id.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("solarsystemid2:") {
        let Some(req_sys) = rest.parse::<u32>().ok() else {
            return Err(deny());
        };
        if actual_sys != req_sys {
            return Err(deny());
        }
        return Ok(());
    }
    if let Some(rest) = lower.strip_prefix("resourcewars:") {
        let Some(req_sys) = rest.parse::<u32>().ok() else {
            return Err(deny());
        };
        if actual_sys != req_sys {
            return Err(deny());
        }
        return Ok(());
    }
    if let Some(rest) = lower.strip_prefix("constellationid:") {
        let Some(req_cid) = rest.parse::<u32>().ok() else {
            return Err(deny());
        };
        if !ctx
            .sde
            .solar_systems
            .get(&actual_sys)
            .is_some_and(|s| s.constellation_id == req_cid)
        {
            return Err(deny());
        }
        return Ok(());
    }
    if let Some(rest) = lower.strip_prefix("regionid:") {
        let Some(req_rid) = rest.parse::<u32>().ok() else {
            return Err(deny());
        };
        if !ctx
            .sde
            .solar_systems
            .get(&actual_sys)
            .is_some_and(|s| s.region_id == req_rid)
        {
            return Err(deny());
        }
        return Ok(());
    }
    if let Some(rest) = lower.strip_prefix("corpid:") {
        let Some(req_corp) = rest.parse::<i64>().ok() else {
            return Err(deny());
        };
        if actual_corp != Some(req_corp) {
            return Err(deny());
        }
        return Ok(());
    }
    if let Some(rest) = lower.strip_prefix("allianceid:") {
        let Some(req_ally) = rest.parse::<i64>().ok() else {
            return Err(deny());
        };
        if actual_ally != Some(req_ally) {
            return Err(deny());
        }
        return Ok(());
    }
    if let Some(rest) = lower.strip_prefix("fleetid:") {
        let Some(req_fleet) = rest.parse::<i64>().ok() else {
            return Err(deny());
        };
        let in_fleet = (target_char == caller_id && session.fleet_id == Some(req_fleet))
            || snap
                .fleets
                .get(&req_fleet)
                .is_some_and(|f| f.members.contains(&target_char));
        if !in_fleet {
            return Err(deny());
        }
        return Ok(());
    }
    if let Some(rest) = lower.strip_prefix("factionid:") {
        let Some(req_fac) = rest.parse::<i64>().ok() else {
            return Err(deny());
        };
        if actual_faction != Some(req_fac) {
            return Err(deny());
        }
        return Ok(());
    }
    let is_priv = lower.starts_with("private:")
        || snap.chat_channels.get(chan_id).is_some_and(|c| c.is_private);
    if is_priv {
        let invited = snap.chat_channels.get(chan_id).is_some_and(|c| {
            c.members.contains(&target_char)
                || (target_char != caller_id
                    && (c.owner_id == Some(caller_id) || c.operators.contains(&caller_id)))
        }) || snap
            .chat_channel_acl
            .get(chan_id)
            .is_some_and(|a| a.allow_list.contains(&target_char));
        if !invited {
            return Err(deny());
        }
    }
    Ok(())
}

pub(super) fn check_channel_or_global_mute(
    ctx: &ServerContext,
    chan_id: &str,
    char_id: i64,
    now_unix: u64,
) -> Option<String> {
    let db_mute = ctx.db.with_snapshot(|snap| {
        snap.chat_channel_acl
            .get(chan_id)
            .and_then(|a| a.muted_until.get(&char_id).cloned())
            .filter(|(until, _)| now_unix < *until)
            .or_else(|| {
                snap.chat_channel_acl
                    .get("global")
                    .and_then(|a| a.muted_until.get(&char_id).cloned())
                    .filter(|(until, _)| now_unix < *until)
            })
    });
    if let Some((_, reason)) = db_mute {
        return Some(reason);
    }
    let s = ctx.social_extras.lock().unwrap();
    if let Some(&(until, ref reason)) = s.muted_in_channel.get(&(chan_id.to_string(), char_id)) {
        if now_unix < until {
            return Some(reason.clone());
        }
    }
    if let Some(&(until, ref reason)) = s.muted_in_channel.get(&("global".to_string(), char_id)) {
        if now_unix < until {
            return Some(reason.clone());
        }
    }
    None
}

#[inline]
fn handle_char_contacts(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    crate::services::character_skills::character_contacts::handle_contacts(
        ctx, session, "charMgr", method, args, kwargs,
    )
}

pub(super) fn handle_contacts_notifications_calendar(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // parity: rpc:charMgr.GetContactList
        ("charMgr", "GetContactList") => handle_char_contacts(ctx, session, method, args, kwargs),
        // parity: rpc:charMgr.AddContact
        ("charMgr", "AddContact") => handle_char_contacts(ctx, session, method, args, kwargs),
        // parity: rpc:charMgr.EditContact, rpc:charMgr.EditContactsRelationshipID
        ("charMgr", "EditContact") | ("charMgr", "EditContactsRelationshipID") => {
            handle_char_contacts(ctx, session, method, args, kwargs)
        }
        // parity: rpc:charMgr.DeleteContacts
        ("charMgr", "DeleteContacts") => handle_char_contacts(ctx, session, method, args, kwargs),
        // parity: rpc:charMgr.BlockOwners, rpc:charMgr.UnblockOwners
        ("charMgr", "BlockOwners") | ("charMgr", "UnblockOwners") => {
            handle_char_contacts(ctx, session, method, args, kwargs)
        }

        ("charMgr", "GetSettingsInfo") => Some(
            ServerContext::require_character(session)
                .map(|_| crate::services::client_login::settings_info_blob()),
        ),

        ("charMgr", "LogSettings") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let isk = args
                .first()
                .and_then(|a| a.as_f64().or_else(|| a.as_i64().map(|v| v as f64)))
                .unwrap_or(2_950.0)
                .clamp(0.0, 1_000_000.0);
            let charge = IskCents::from_f64_isk(isk);
            if let Err(e) = ctx.db.try_with_mut_snapshot(|snap| {
                snap.character_cspa_charges.insert(char_id, charge);
                Ok(())
            }) {
                return Some(Err(e));
            }
            ctx.social_extras
                .lock()
                .unwrap()
                .cspa_charges
                .insert(char_id, charge);
            Some(Ok(PyRep::Real(isk)))
        }

        ("charMgr", "CSPAChargedAction") | ("charMgr", "CSPAChargedActionForMany") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_ids: Vec<i64> = if method == "CSPAChargedActionForMany" {
                let list_arg = args
                    .iter()
                    .find(|a| matches!(a, PyRep::List(_) | PyRep::Tuple(_)))
                    .or_else(|| args.first());
                super::extract_id_list(list_arg)
            } else {
                args.iter()
                    .rev()
                    .find_map(|a| a.as_i64())
                    .into_iter()
                    .collect()
            };
            let mut total_cspa = IskCents(0);
            {
                let s = ctx.social_extras.lock().unwrap();
                for &rec_id in &target_ids {
                    if rec_id == char_id || rec_id <= 0 {
                        continue;
                    }
                    let friendly = s
                        .contacts
                        .get(&rec_id)
                        .and_then(|m| m.get(&char_id))
                        .is_some_and(|&(st, _, _)| st > 0.0)
                        || ctx.db.with_snapshot(|snap| {
                            if snap.char_contacts.get(&rec_id).is_some_and(|list| {
                                list.iter().any(|c| {
                                    c.contact_id == char_id && !c.is_blocked && c.standing > 0.0
                                })
                            }) {
                                return true;
                            }
                            let c1 = snap.characters.get(&char_id);
                            let c2 = snap.characters.get(&rec_id);
                            match (c1, c2) {
                                (Some(a), Some(b)) => {
                                    a.corporation_id == b.corporation_id
                                        || (a.alliance_id.is_some()
                                            && a.alliance_id == b.alliance_id)
                                }
                                _ => false,
                            }
                        });
                    if !friendly {
                        let charge = ctx
                            .db
                            .with_snapshot(|snap| {
                                snap.character_cspa_charges.get(&rec_id).copied()
                            })
                            .or_else(|| s.cspa_charges.get(&rec_id).copied())
                            .unwrap_or_else(|| IskCents::from_f64_isk(2_950.0));
                        if charge.0 > 0 {
                            total_cspa = total_cspa.checked_add(charge).unwrap_or(total_cspa);
                        }
                    }
                }
            }
            if total_cspa.0 > 0 {
                let res = ctx.db.try_with_mut_snapshot(|snap| {
                    let Some(ch) = snap.characters.get_mut(&char_id) else {
                        return Err(BridgeRpcError {
                            code: "CharacterNotFound".into(),
                            message: format!("Character {char_id} not found"),
                        });
                    };
                    ch.balance =
                        ch.balance
                            .checked_sub(total_cspa)
                            .ok_or_else(|| BridgeRpcError {
                                code: "InsufficientFundsForCSPA".into(),
                                message: "Insufficient ISK to pay recipient CSPA charge".into(),
                            })?;
                    Ok(())
                });
                if let Err(e) = res {
                    return Some(Err(e));
                }
            }
            Some(Ok(PyRep::Real(total_cspa.to_wire_f64())))
        }

        ("charMgr", "SetNote") | ("charMgr", "GetNote") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            if method == "SetNote" {
                let note = args.get(1).and_then(|a| a.as_str()).unwrap_or("");
                if let Err(e) = ctx.db.try_with_mut_snapshot(|snap| {
                    snap.char_notes.insert((char_id, item_id), note.to_string());
                    Ok(())
                }) {
                    return Some(Err(e));
                }
                ctx.social_extras
                    .lock()
                    .unwrap()
                    .char_notes
                    .insert((char_id, item_id), note.to_string());
                Some(Ok(PyRep::Bool(true)))
            } else {
                let note = ctx
                    .db
                    .with_snapshot(|snap| snap.char_notes.get(&(char_id, item_id)).cloned())
                    .or_else(|| {
                        ctx.social_extras
                            .lock()
                            .unwrap()
                            .char_notes
                            .get(&(char_id, item_id))
                            .cloned()
                    })
                    .unwrap_or_default();
                Some(Ok(PyRep::String(note)))
            }
        }

        _ => None,
    }
}
