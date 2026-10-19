//! `auth_cache` — Domain 1: Session, Authentication, Object Caching, Config, & Quasar Gateway (`§3.1`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;
const V24_01_VERSION: f64 = 24.01;
const V24_01_BUILD: i64 = 3_619_879;

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("machoNet", "GetInitVals") => {
            let mut sorted_svcs: Vec<&String> = ctx.cataloged_services.iter().collect();
            sorted_svcs.sort();
            let service_entries: Vec<(PyRep, PyRep)> = sorted_svcs
                .into_iter()
                .map(|svc| (PyRep::String(svc.clone()), PyRep::String("sol".into())))
                .collect();
            let cluster_info = vec![(
                PyRep::String("catalogedServices".into()),
                PyRep::Integer(ctx.cataloged_services.len() as i64),
            )];
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Dict(service_entries),
                PyRep::Dict(cluster_info),
            ])))
        }

        ("machoNet", "GetServiceInfo") => {
            let mut sorted_svcs: Vec<&String> = ctx.cataloged_services.iter().collect();
            sorted_svcs.sort();
            let service_entries: Vec<(PyRep, PyRep)> = sorted_svcs
                .into_iter()
                .map(|svc| (PyRep::String(svc.clone()), PyRep::String("sol".into())))
                .collect();
            Some(Ok(PyRep::Dict(service_entries)))
        }

        ("machoNet", "GetTime") => {
            let ticks = ctx.now_blueos_filetime_ticks();
            Some(Ok(PyRep::Integer(ticks)))
        }

        ("cache", "GetServerVersionAndBuild") => {
            let ticks = ctx.now_blueos_filetime_ticks();
            Some(Ok(PyRep::keyval([
                ("version", PyRep::Real(V24_01_VERSION)),
                ("build", PyRep::Integer(V24_01_BUILD)),
                ("sdeTypes", PyRep::Integer(ctx.sde.types.len() as i64)),
                ("timestamp", PyRep::Integer(ticks)),
            ])))
        }

        ("authentication", "Login") => {
            let username = args.first().and_then(|a| a.as_str()).unwrap_or("");
            let token = args.get(1).and_then(|a| a.as_str()).unwrap_or("");
            let now_unix = ctx.now_unix();
            let res =
                ctx.db
                    .authenticate_at(username, token, now_unix)
                    .map_err(|e| BridgeRpcError {
                        code: match e {
                            eve_db::DbError::AccountBanned { .. } => "AccountBanned".into(),
                            eve_db::DbError::AccountSuspended { .. } => "AccountSuspended".into(),
                            _ => "AuthenticationFailed".into(),
                        },
                        message: e.to_string(),
                    });
            let acc = match res {
                Ok(a) => a,
                Err(e) => return Some(Err(e)),
            };

            if let Some(prev_uid) = session.user_id {
                ctx.db.release_online_lock(prev_uid, session.session_id);
            }
            ctx.release_session_bindings(session.session_id);
            ctx.db
                .clear_revocation_on_fresh_login(acc.user_id, session.session_id);
            if ctx.db.get_online_session(acc.user_id).is_none() {
                let _ = ctx.db.acquire_online_lock(
                    acc.user_id,
                    session.session_id,
                    None,
                    false,
                    now_unix,
                );
            }
            session.user_id = Some(acc.user_id);
            session.role = acc.role;
            session.character_id = None;
            session.corporation_id = None;
            session.alliance_id = None;
            session.station_id = None;
            session.structure_id = None;
            session.solar_system_id = None;
            session.constellation_id = None;
            session.region_id = None;
            session.ship_id = None;
            session.race_id = None;
            session.corp_role = 0;
            session.fleet_id = None;
            session.war_faction_id = None;

            Some(Ok(PyRep::keyval([
                ("userID", PyRep::Integer(acc.user_id as i64)),
                ("role", PyRep::Integer(acc.role as i64)),
            ])))
        }

        ("authentication", "SetLanguageID") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let Some(lang) = args.first().and_then(|a| a.as_str()).map(str::trim).filter(|s| !s.is_empty()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidLanguageID".into(),
                    message: "SetLanguageID requires a non-empty language code".into(),
                }));
            };
            session.language_id = lang.to_string();
            Some(Ok(PyRep::None))
        }

        ("authentication", "Logout") => {
            if let Some(uid) = session.user_id {
                ctx.db.release_online_lock(uid, session.session_id);
            }
            ctx.release_session_bindings(session.session_id);
            session.user_id = None;
            session.role = 0;
            session.character_id = None;
            session.corporation_id = None;
            session.alliance_id = None;
            session.station_id = None;
            session.structure_id = None;
            session.solar_system_id = None;
            session.constellation_id = None;
            session.region_id = None;
            session.ship_id = None;
            Some(Ok(PyRep::keyval([("loggedOut", PyRep::Bool(true))])))
        }

        ("multiLoginBlocker", "Login") => {
            let uid = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix();
            // V24 client `characterSelection.pyj` (`_CanLogin`) passes `computerhash` as `args[0]` (str/bytes)
            // and expects a boolean `True` (allowed) or `False` (blocked -> opens `MultiLoginBlockedWindow`).
            let computer_hash_arg: Option<String> = match args.first() {
                Some(PyRep::String(s)) => Some(s.clone()),
                Some(PyRep::Bytes(b)) => Some(String::from_utf8_lossy(b).into_owned()),
                _ => None,
            };
            if let Some(computer_hash) = computer_hash_arg {
                let allow_displace = args
                    .get(1)
                    .and_then(|a| a.as_bool())
                    .or_else(|| _kwargs.get("displace").and_then(|a| a.as_bool()))
                    .unwrap_or(false);
                if ctx
                    .db
                    .has_alpha_computer_hash_conflict(uid, session.session_id, &computer_hash)
                {
                    return Some(Ok(PyRep::Bool(false)));
                }
                match ctx.db.acquire_online_lock_with_computer_hash(
                    uid,
                    session.session_id,
                    session.character_id,
                    Some(computer_hash),
                    allow_displace,
                    now_unix,
                ) {
                    Ok(displaced) => {
                        if let Some(ref d) = displaced {
                            ctx.disconnect_session(
                                d.session_id,
                                "ACL_DISPLACED",
                                "Displaced by concurrent login on same account",
                            );
                        }
                        Some(Ok(PyRep::Bool(true)))
                    }
                    Err(eve_db::DbError::AlreadyLoggedIn { .. }) => Some(Ok(PyRep::Bool(false))),
                    Err(e) => {
                        let code = match e {
                            eve_db::DbError::AccountBanned { .. } => "AccountBanned",
                            eve_db::DbError::AccountSuspended { .. } => "AccountSuspended",
                            eve_db::DbError::ValidationError(_) => "SessionKicked",
                            _ => "AlreadyLoggedIn",
                        };
                        Some(Err(BridgeRpcError {
                            code: code.into(),
                            message: e.to_string(),
                        }))
                    }
                }
            } else {
                let allow_displace = args.first().and_then(|a| a.as_bool()).unwrap_or(false);
                match ctx.db.acquire_online_lock(
                    uid,
                    session.session_id,
                    session.character_id,
                    allow_displace,
                    now_unix,
                ) {
                    Ok(displaced) => {
                        if let Some(ref d) = displaced {
                            ctx.disconnect_session(
                                d.session_id,
                                "ACL_DISPLACED",
                                "Displaced by concurrent login on same account",
                            );
                        }
                        Some(Ok(PyRep::keyval([
                            ("userID", PyRep::Integer(uid as i64)),
                            ("sessionID", PyRep::Integer(session.session_id as i64)),
                            (
                                "displacedSessionID",
                                displaced
                                    .map(|d| PyRep::Integer(d.session_id as i64))
                                    .unwrap_or(PyRep::None),
                            ),
                        ])))
                    }
                    Err(e) => {
                        let code = match e {
                            eve_db::DbError::AccountBanned { .. } => "AccountBanned",
                            eve_db::DbError::AccountSuspended { .. } => "AccountSuspended",
                            eve_db::DbError::ValidationError(_) => "SessionKicked",
                            _ => "AlreadyLoggedIn",
                        };
                        Some(Err(BridgeRpcError {
                            code: code.into(),
                            message: e.to_string(),
                        }))
                    }
                }
            }
        }

        ("subscriptionMgr", "GetCloneGrade") => {
            let uid = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let (clone_grade_u8, race_id) = session
                .character_id
                .and_then(|cid| ctx.db.get_character(cid).ok())
                .map(|c| (c.clone_grade, c.race_id))
                .unwrap_or((1, 1));
            let skill_caps_count = ctx
                .sde
                .clone_grades
                .get(&race_id)
                .or_else(|| ctx.sde.clone_grades.get(&(clone_grade_u8 as u32)))
                .map(|g| g.alpha_skill_caps.len())
                .unwrap_or(0);
            Some(Ok(PyRep::keyval([
                ("userID", PyRep::Integer(uid as i64)),
                ("cloneGrade", PyRep::Integer(clone_grade_u8 as i64)),
                ("alphaSkillCapsCount", PyRep::Integer(skill_caps_count as i64)),
            ])))
        }

        ("loginRewardFacilities", "get_my_clone_grade") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let clone_grade_u8 = session
                .character_id
                .and_then(|cid| ctx.db.get_character(cid).ok())
                .map(|c| c.clone_grade)
                .unwrap_or(1);
            Some(Ok(PyRep::Integer(clone_grade_u8 as i64)))
        }

        ("slash", "SlashCmd") => {
            let (_, cid) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "slash.SlashCmd requires GM/Admin role (ROLE_GMS/ROLE_GML/ROLE_GMH/ROLE_ADMIN)".into(),
                }));
            }
            let cmd = args.first().and_then(|a| a.as_str()).unwrap_or("").trim();
            if let Some(rest) = cmd.strip_prefix("/giveisk ") {
                let Ok(amount) = rest.trim().parse::<i64>() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /giveisk <positive_integer_amount>".into(),
                    }));
                };
                if amount <= 0 {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Amount for /giveisk must be positive".into(),
                    }));
                }
                let new_bal = ctx.db.try_with_mut_snapshot(|snap| {
                    let ch = snap.characters.get_mut(&cid).ok_or_else(|| BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {cid} not found"),
                    })?;
                    let add = IskCents::from_isk(amount);
                    ch.balance = ch.balance.checked_add(add).ok_or_else(|| BridgeRpcError {
                        code: "BalanceOverflow".into(),
                        message: "Adding ISK would overflow balance".into(),
                    })?;
                    Ok(ch.balance.to_wire_f64())
                });
                match new_bal {
                    Ok(bal) => {
                        let _ = ctx.notify_account_change(cid, "cash", cid, bal);
                        Some(Ok(PyRep::keyval([
                            ("command", PyRep::String(cmd.to_string())),
                            ("newBalanceISK", PyRep::Real(bal)),
                        ])))
                    }
                    Err(e) => Some(Err(e)),
                }
            } else if let Some(rest) = cmd.strip_prefix("/ban ") {
                let mut parts = rest.trim().splitn(3, ' ');
                let Some(username) = parts.next().map(str::trim).filter(|s| !s.is_empty()) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /ban <username> <duration_seconds> [reason]".into(),
                    }));
                };
                let Some(dur_str) = parts.next().map(str::trim) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /ban <username> <duration_seconds> [reason]".into(),
                    }));
                };
                let Ok(duration_secs) = dur_str.parse::<u64>() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Duration for /ban must be a positive integer in seconds".into(),
                    }));
                };
                if duration_secs == 0 {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Duration for /ban must be greater than zero".into(),
                    }));
                }
                let reason = parts
                    .next()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("Banned by GM");
                let until_unix = ctx.now_unix().saturating_add(duration_secs);
                let live_sess = ctx
                    .db
                    .with_snapshot(|s| s.accounts.get(username).map(|a| a.user_id))
                    .and_then(|uid| ctx.db.get_online_session(uid));
                match ctx.db.ban_account(username, until_unix, reason) {
                    Ok(uid) => {
                        ctx.release_user_bindings(uid);
                        if let Some(ref s) = live_sess {
                            ctx.disconnect_session(
                                s.session_id,
                                "AccountBanned",
                                &format!("Account banned until unix={until_unix}: {reason}"),
                            );
                        }
                        Some(Ok(PyRep::keyval([
                            ("command", PyRep::String(cmd.to_string())),
                            ("username", PyRep::String(username.to_string())),
                            ("userID", PyRep::Integer(uid as i64)),
                            ("bannedUntilUnix", PyRep::Integer(until_unix as i64)),
                            ("reason", PyRep::String(reason.to_string())),
                            (
                                "kickedSessionID",
                                live_sess
                                    .map(|s| PyRep::Integer(s.session_id as i64))
                                    .unwrap_or(PyRep::None),
                            ),
                        ])))
                    }
                    Err(e) => Some(Err(BridgeRpcError {
                        code: "BanFailed".into(),
                        message: e.to_string(),
                    })),
                }
            } else if let Some(rest) = cmd.strip_prefix("/unban ") {
                let username = rest.trim();
                if username.is_empty() {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /unban <username>".into(),
                    }));
                }
                match ctx.db.unban_account(username) {
                    Ok(uid) => Some(Ok(PyRep::keyval([
                        ("command", PyRep::String(cmd.to_string())),
                        ("username", PyRep::String(username.to_string())),
                        ("userID", PyRep::Integer(uid as i64)),
                        ("unbanned", PyRep::Bool(true)),
                    ]))),
                    Err(e) => Some(Err(BridgeRpcError {
                        code: "UnbanFailed".into(),
                        message: e.to_string(),
                    })),
                }
            } else if let Some(rest) = cmd.strip_prefix("/suspend ") {
                let mut parts = rest.trim().splitn(3, ' ');
                let Some(username) = parts.next().map(str::trim).filter(|s| !s.is_empty()) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /suspend <username> <duration_seconds> [reason]".into(),
                    }));
                };
                let Some(dur_str) = parts.next().map(str::trim) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /suspend <username> <duration_seconds> [reason]".into(),
                    }));
                };
                let Ok(duration_secs) = dur_str.parse::<u64>() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Duration for /suspend must be a positive integer in seconds".into(),
                    }));
                };
                if duration_secs == 0 {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Duration for /suspend must be greater than zero".into(),
                    }));
                }
                let reason = parts
                    .next()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("Suspended by GM");
                let until_unix = ctx.now_unix().saturating_add(duration_secs);
                let live_sess = ctx
                    .db
                    .with_snapshot(|s| s.accounts.get(username).map(|a| a.user_id))
                    .and_then(|uid| ctx.db.get_online_session(uid));
                match ctx.db.suspend_account(username, until_unix, reason) {
                    Ok(uid) => {
                        ctx.release_user_bindings(uid);
                        if let Some(ref s) = live_sess {
                            ctx.disconnect_session(
                                s.session_id,
                                "AccountSuspended",
                                &format!("Account suspended until unix={until_unix}: {reason}"),
                            );
                        }
                        Some(Ok(PyRep::keyval([
                            ("command", PyRep::String(cmd.to_string())),
                            ("username", PyRep::String(username.to_string())),
                            ("userID", PyRep::Integer(uid as i64)),
                            ("suspendedUntilUnix", PyRep::Integer(until_unix as i64)),
                            ("reason", PyRep::String(reason.to_string())),
                            (
                                "kickedSessionID",
                                live_sess
                                    .map(|s| PyRep::Integer(s.session_id as i64))
                                    .unwrap_or(PyRep::None),
                            ),
                        ])))
                    }
                    Err(e) => Some(Err(BridgeRpcError {
                        code: "SuspendFailed".into(),
                        message: e.to_string(),
                    })),
                }
            } else if let Some(rest) = cmd.strip_prefix("/unsuspend ") {
                let username = rest.trim();
                if username.is_empty() {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /unsuspend <username>".into(),
                    }));
                }
                match ctx.db.unsuspend_account(username) {
                    Ok(uid) => Some(Ok(PyRep::keyval([
                        ("command", PyRep::String(cmd.to_string())),
                        ("username", PyRep::String(username.to_string())),
                        ("userID", PyRep::Integer(uid as i64)),
                        ("unsuspended", PyRep::Bool(true)),
                    ]))),
                    Err(e) => Some(Err(BridgeRpcError {
                        code: "UnsuspendFailed".into(),
                        message: e.to_string(),
                    })),
                }
            } else if let Some(rest) = cmd.strip_prefix("/kick ") {
                let mut parts = rest.trim().splitn(2, ' ');
                let Some(username) = parts.next().map(str::trim).filter(|s| !s.is_empty()) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /kick <username> [reason]".into(),
                    }));
                };
                let reason = parts
                    .next()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("Kicked by GM");
                let Some((uid, is_admin)) = ctx.db.with_snapshot(|s| {
                    s.accounts.get(username).map(|a| {
                        (
                            a.user_id,
                            (a.role & eve_db::service_roles::ROLE_ADMIN) != 0,
                        )
                    })
                }) else {
                    return Some(Err(BridgeRpcError {
                        code: "UnknownAccount".into(),
                        message: format!("Unknown account '{username}'"),
                    }));
                };
                if is_admin {
                    return Some(Err(BridgeRpcError {
                        code: "KickFailed".into(),
                        message: format!("Cannot kick administrator account '{username}'"),
                    }));
                }
                let kicked_sess = ctx.db.kick_user(uid, reason, ctx.now_unix());
                ctx.release_user_bindings(uid);
                if let Some(ref s) = kicked_sess {
                    ctx.disconnect_session(s.session_id, "SessionKicked", reason);
                }
                Some(Ok(PyRep::keyval([
                    ("command", PyRep::String(cmd.to_string())),
                    ("username", PyRep::String(username.to_string())),
                    ("userID", PyRep::Integer(uid as i64)),
                    (
                        "kickedSessionID",
                        kicked_sess
                            .map(|s| PyRep::Integer(s.session_id as i64))
                            .unwrap_or(PyRep::None),
                    ),
                    ("reason", PyRep::String(reason.to_string())),
                ])))
            } else if let Some(rest) = cmd.strip_prefix("/giveitem ") {
                let mut parts = rest.trim().split_whitespace();
                let Some(type_id_str) = parts.next() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /giveitem <type_id> [quantity]".into(),
                    }));
                };
                let Ok(type_id) = type_id_str.parse::<u32>() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Invalid type_id for /giveitem".into(),
                    }));
                };
                let qty: i32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
                if qty <= 0 {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Quantity for /giveitem must be positive".into(),
                    }));
                }
                let Some(inv_type) = ctx.sde.types.get(&type_id) else {
                    return Some(Err(BridgeRpcError {
                        code: "TypeNotFound".into(),
                        message: format!("Type ID {type_id} not found in SDE"),
                    }));
                };
                let is_ship = inv_type.category_id == 6;
                let res = ctx.db.try_with_mut_snapshot(|snap| {
                    let ch = snap.characters.get(&cid).ok_or_else(|| BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {cid} not found"),
                    })?;
                    let (location_id, flag) = if let Some(st_id) = ch.station_id.or(ch.structure_id) {
                        (st_id, InventoryFlag::Hangar)
                    } else {
                        (ch.active_ship_id, InventoryFlag::Cargo)
                    };
                    snap.next_entity_id += 1;
                    let item_id = snap.next_entity_id;
                    let item = InventoryItemRecord {
                        item_id,
                        type_id,
                        owner_id: cid,
                        location_id,
                        flag,
                        quantity: qty,
                        singleton: is_ship,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    };
                    snap.items.insert(item_id, item);
                    Ok((item_id, location_id))
                });
                match res {
                    Ok((item_id, loc_id)) => Some(Ok(PyRep::keyval([
                        ("command", PyRep::String(cmd.to_string())),
                        ("itemID", PyRep::Integer(item_id)),
                        ("typeID", PyRep::Integer(type_id as i64)),
                        ("quantity", PyRep::Integer(qty as i64)),
                        ("locationID", PyRep::Integer(loc_id)),
                    ]))),
                    Err(e) => Some(Err(e)),
                }
            } else if let Some(rest) = cmd.strip_prefix("/setskill ") {
                let mut parts = rest.trim().split_whitespace();
                let Some(skill_id_str) = parts.next() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /setskill <skill_id> <level>".into(),
                    }));
                };
                let Ok(skill_id) = skill_id_str.parse::<u32>() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Invalid skill_id for /setskill".into(),
                    }));
                };
                let Some(lvl_str) = parts.next() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /setskill <skill_id> <level>".into(),
                    }));
                };
                let Ok(lvl) = lvl_str.parse::<u8>() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Invalid level for /setskill".into(),
                    }));
                };
                if lvl > 5 {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Level for /setskill must be between 0 and 5".into(),
                    }));
                }
                let Some(skill_type) = ctx.sde.types.get(&skill_id) else {
                    return Some(Err(BridgeRpcError {
                        code: "SkillNotFound".into(),
                        message: format!("Skill type ID {skill_id} not found in SDE"),
                    }));
                };
                if skill_type.category_id != 16 {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSkillType".into(),
                        message: format!("Type {skill_id} is not a skill (category 16)"),
                    }));
                }
                let rank = skill_type
                    .default_attributes
                    .get(&275)
                    .copied()
                    .unwrap_or(1.0);
                let target_sp = crate::formulas::sp_for_skill_level(rank, lvl);

                let res = ctx.db.try_with_mut_snapshot(|snap| {
                    let ch = snap.characters.get_mut(&cid).ok_or_else(|| BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {cid} not found"),
                    })?;
                    let skills_map = snap.skills.entry(cid).or_default();
                    let prev_sp = skills_map.get(&skill_id).map(|s| s.skill_points).unwrap_or(0.0);
                    let sp_diff = target_sp - prev_sp;
                    ch.skill_points = (ch.skill_points + sp_diff).max(0.0);

                    if lvl == 0 {
                        skills_map.remove(&skill_id);
                    } else {
                        skills_map.insert(
                            skill_id,
                            eve_db::TrainedSkillRecord {
                                type_id: skill_id,
                                level: lvl,
                                skill_points: target_sp,
                            },
                        );
                    }
                    let active_ship = ch.active_ship_id;
                    Ok((target_sp, ch.skill_points, active_ship))
                });
                match res {
                    Ok((sp, total_sp, active_ship)) => {
                        ctx.invalidate_ship_fitting(active_ship);
                        Some(Ok(PyRep::keyval([
                            ("command", PyRep::String(cmd.to_string())),
                            ("skillID", PyRep::Integer(skill_id as i64)),
                            ("level", PyRep::Integer(lvl as i64)),
                            ("skillPoints", PyRep::Real(sp)),
                            ("characterTotalSP", PyRep::Real(total_sp)),
                        ])))
                    }
                    Err(e) => Some(Err(e)),
                }
            } else if let Some(rest) = cmd.strip_prefix("/spawnnpc ") {
                let type_id_str = rest.trim();
                let Ok(type_id) = type_id_str.parse::<u32>() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /spawnnpc <type_id>".into(),
                    }));
                };
                let Some(npc_type) = ctx.sde.types.get(&type_id) else {
                    return Some(Err(BridgeRpcError {
                        code: "TypeNotFound".into(),
                        message: format!("NPC type ID {type_id} not found in SDE"),
                    }));
                };
                let (sys_id, ship_id) = ctx.db.with_snapshot(|snap| {
                    snap.characters.get(&cid).map(|c| (c.solar_system_id, c.active_ship_id))
                }).unwrap_or((30000142, 0));

                let mass = npc_type.mass.max(1.0);
                let radius = npc_type.radius.max(1.0);
                let handle = ctx.systems.get_or_spawn(sys_id);
                let origin = if ship_id > 0 {
                    handle
                        .get_ball_snapshot(ship_id)
                        .await
                        .ok()
                        .flatten()
                        .map(|(_, b)| b.position)
                        .unwrap_or(DVec3::ZERO)
                } else {
                    DVec3::ZERO
                };

                let npc_id = ctx.db.with_mut_snapshot(|snap| {
                    snap.next_entity_id += 1;
                    700_000_000_i64.saturating_add(snap.next_entity_id)
                });

                let _ = handle
                    .spawn_ball_with_combat(
                        Ball {
                            id: npc_id,
                            type_id,
                            position: origin + DVec3::new(1_000.0, 0.0, 0.0),
                            velocity: DVec3::ZERO,
                            max_velocity: 350.0,
                            inertia: 1.0,
                            mass,
                            radius,
                            speed_fraction: 0.0,
                            mode: MovementMode::Stop,
                        },
                        ctx.build_type_combat_state(type_id),
                    )
                    .await;

                Some(Ok(PyRep::keyval([
                    ("command", PyRep::String(cmd.to_string())),
                    ("npcID", PyRep::Integer(npc_id)),
                    ("typeID", PyRep::Integer(type_id as i64)),
                    ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ])))
            } else if let Some(rest) = cmd.strip_prefix("/teleport ") {
                let sys_id_str = rest.trim();
                let Ok(dest_sys_id) = sys_id_str.parse::<u32>() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /teleport <solar_system_id>".into(),
                    }));
                };
                if !ctx.sde.solar_systems.contains_key(&dest_sys_id) {
                    return Some(Err(BridgeRpcError {
                        code: "SolarSystemNotFound".into(),
                        message: format!("Solar system {dest_sys_id} not found in SDE"),
                    }));
                }
                let (old_sys_id, ship_id) = ctx.db.with_snapshot(|snap| {
                    snap.characters.get(&cid).map(|c| (c.solar_system_id, c.active_ship_id))
                }).unwrap_or((30000142, 0));

                let updated_ch = match ctx.db.set_character_solar_system(cid, dest_sys_id, &ctx.sde) {
                    Ok(c) => c,
                    Err(e) => return Some(Err(BridgeRpcError {
                        code: "TeleportFailed".into(),
                        message: e.to_string(),
                    })),
                };

                if old_sys_id != dest_sys_id && ship_id > 0 {
                    let old_handle = ctx.systems.get_or_spawn(old_sys_id);
                    let existing_ball = old_handle.remove_ball(ship_id).await;
                    let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
                    let mut new_ball = existing_ball.unwrap_or(Ball {
                        id: ship_id,
                        type_id: 601,
                        position: DVec3::ZERO,
                        velocity: DVec3::ZERO,
                        max_velocity: 300.0,
                        inertia: 0.5,
                        mass: 1_000_000.0,
                        radius: 50.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    });
                    new_ball.position = DVec3::new(10_000.0, 0.0, 0.0);
                    new_ball.velocity = DVec3::ZERO;
                    let combat_state = ctx.build_ship_combat_state(cid, ship_id, new_ball.type_id);
                    let slim_meta = ctx.build_ship_slim_meta(cid, new_ball.type_id);
                    let _ = new_handle.spawn_ball_with_meta(new_ball, combat_state, slim_meta).await;
                }

                session.solar_system_id = Some(dest_sys_id);
                session.constellation_id = Some(updated_ch.constellation_id);
                session.region_id = Some(updated_ch.region_id);
                session.station_id = None;
                session.structure_id = None;

                Some(Ok(PyRep::keyval([
                    ("command", PyRep::String(cmd.to_string())),
                    ("characterID", PyRep::Integer(cid)),
                    ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
                    ("constellationID", PyRep::Integer(updated_ch.constellation_id as i64)),
                    ("regionID", PyRep::Integer(updated_ch.region_id as i64)),
                ])))
            } else {
                Some(Err(BridgeRpcError {
                    code: "UnknownSlashCommand".into(),
                    message: format!("Unknown or unsupported slash command '{cmd}'"),
                }))
            }
        }

        _ => super::config_service::handle(ctx, session, service, method, args, _kwargs),
    }
}
