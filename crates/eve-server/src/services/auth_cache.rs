//! `auth_cache` — Domain 1: Session, Authentication, Object Caching, Config, & Quasar Gateway (`§3.1`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

pub static GLOBAL_CONFIG_VALUES: LazyLock<RwLock<HashMap<String, PyRep>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));
const V24_01_VERSION: f64 = 24.01;
pub const V24_01_BUILD: i64 = 3_624_425;

pub fn get_init_vals(ctx: &ServerContext) -> PyRep {
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
    PyRep::Tuple(vec![
        PyRep::Dict(service_entries),
        PyRep::Dict(cluster_info),
    ])
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
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

        ("machoNet", "NoBlock") => {
            // const-ok: machoNet.NoBlock is a keepalive ping returning true
            Some(Ok(PyRep::Bool(true)))
        }

        ("machoNet", "GetCachedObject") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let (shared, object_id) = match (args.first(), args.get(1)) {
                (Some(PyRep::Bool(b)), Some(oid)) => (*b, oid),
                (Some(PyRep::Integer(i)), Some(oid)) => (*i != 0, oid),
                (Some(oid), None) => (true, oid),
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "machoNet.GetCachedObject requires (shared, objectID) or (objectID,)".into(),
                    }));
                }
            };
            let Some(rec) = ctx.get_or_populate_cached_object(object_id, shared) else {
                return Some(Err(BridgeRpcError {
                    code: "CachedObjectNotFound".into(),
                    message: format!("Cached object {:?} is not registered", object_id),
                }));
            };
            Some(Ok(rec.to_cached_object_pyrep()))
        }

        ("machoNet", "ForwardCharacterNotification") => {
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "machoNet.ForwardCharacterNotification requires GM privileges".into(),
                }));
            }
            let Some(char_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "ForwardCharacterNotification requires charID as first argument".into(),
                }));
            };
            let notif_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("OnNotificationReceived");
            let is_permitted = session.has_gm_role()
                || session.character_id == Some(char_id)
                || matches!(notif_name, "OnNotificationReceived" | "OnNewNotificationReceived");
            if !is_permitted {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "ForwardCharacterNotification targeting another character with unprivileged notification requires GM role".into(),
                }));
            }
            let payload: Vec<PyRep> = match args.get(2) {
                Some(PyRep::List(l)) | Some(PyRep::Tuple(l)) => l.clone(),
                Some(other) => vec![other.clone()],
                None => Vec::new(),
            };
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                notif_name,
                "charid",
                payload,
            );
            Some(Ok(PyRep::Bool(true)))
        }

        ("machoNet", "ReloadClientCodeHash") => {
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "ReloadClientCodeHash requires GM role".into(),
                }));
            }
            Some(Ok(PyRep::keyval([
                ("hash", PyRep::String("v24_canonical_code_hash_verified".into())),
                ("reloaded", PyRep::Bool(true)),
                ("timestamp", PyRep::Integer(ctx.now_blueos_filetime_ticks())),
            ])))
        }

        ("machoNet", "SetGlobalConfigValue") => {
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "SetGlobalConfigValue requires GM role".into(),
                }));
            }
            let key = args.first().and_then(|a| a.as_str()).unwrap_or("").to_string();
            if key.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "SetGlobalConfigValue requires config key".into(),
                }));
            }
            let val = args.get(1).cloned().unwrap_or(PyRep::None);
            GLOBAL_CONFIG_VALUES.write().unwrap().insert(key, val);
            Some(Ok(PyRep::Bool(true)))
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
            if args.len() != 2 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidLoginArguments".into(),
                    message: "authentication.Login expects (username, password)".into(),
                }));
            }
            let username = args.first().and_then(|a| a.as_str()).unwrap_or("");
            let token = args.get(1).and_then(|a| a.as_str()).unwrap_or("");
            let now_unix = ctx.now_unix();
            let res =
                ctx.db
                    .authenticate_at(username, token, now_unix)
                    .map_err(|e| BridgeRpcError {
                        code: match e {
                            eve_db::DbError::AccountBanned { .. } => "UserBanned".into(),
                            eve_db::DbError::AccountSuspended { .. } => "AccountSuspended".into(),
                            _ => "AuthenticationFailed".into(),
                        },
                        message: e.to_string(),
                    });
            let acc = match res {
                Ok(a) => a,
                Err(e) => return Some(Err(e)),
            };

            if let Some(prev_cid) = session.character_id {
                crate::services::fleet_social::fleet_social_notify::handle_character_disconnect(ctx, prev_cid);
            }
            if let Some(prev_uid) = session.user_id {
                if prev_uid != acc.user_id {
                    ctx.db.release_online_lock(prev_uid, session.session_id);
                }
            }
            ctx.release_session_bindings(session.session_id);
            ctx.db
                .clear_revocation_on_fresh_login(acc.user_id, session.session_id);

            if let Ok(Some(displaced)) = ctx.db.acquire_online_lock(
                acc.user_id,
                session.session_id,
                None,
                true,
                now_unix,
            ) {
                if let Some(disp_cid) = displaced.character_id {
                    crate::services::fleet_social::fleet_social_notify::handle_character_disconnect(ctx, disp_cid);
                }
                ctx.disconnect_session(
                    displaced.session_id,
                    "ACL_DISPLACED",
                    "Displaced by concurrent login on same account",
                );
                ctx.release_session_bindings(displaced.session_id);
            }
            let prev_role = session.role;
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
                (
                    "sessionChange",
                    PyRep::Dict(vec![
                        (
                            PyRep::String("userid".into()),
                            PyRep::Tuple(vec![PyRep::None, PyRep::Integer(acc.user_id as i64)]),
                        ),
                        (
                            PyRep::String("role".into()),
                            PyRep::Tuple(vec![
                                PyRep::Integer(prev_role as i64),
                                PyRep::Integer(acc.role as i64),
                            ]),
                        ),
                    ]),
                ),
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
            Box::pin(super::account_services::handle(
                ctx,
                session,
                "authentication",
                "Logout",
                args,
                kwargs,
            ))
            .await
        }

        ("multiLoginBlocker", "Login") => {
            let uid = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix();
            if args.len() != 1 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidComputerHash".into(),
                    message: "multiLoginBlocker.Login expects a single computerhash argument".into(),
                }));
            }
            // V24 client `characterSelection.pyj` (`_CanLogin`) passes `computerhash` as `args[0]` (str/bytes)
            // and expects a boolean `True` (allowed) or `False` (blocked -> opens `MultiLoginBlockedWindow`).
            let computer_hash = match args.first() {
                Some(PyRep::String(s)) if !s.trim().is_empty() => s.clone(),
                Some(PyRep::Bytes(b)) if !b.is_empty() => String::from_utf8_lossy(b).into_owned(),
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidComputerHash".into(),
                        message: "multiLoginBlocker.Login requires a non-empty computerhash string"
                            .into(),
                    }))
                }
            };
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
                false,
                now_unix,
            ) {
                Ok(_) => Some(Ok(PyRep::Bool(true))),
                Err(eve_db::DbError::AccountBanned { .. }) => Some(Err(BridgeRpcError {
                    code: "UserBanned".into(),
                    message: "Account is banned".into(),
                })),
                Err(eve_db::DbError::AccountSuspended { .. }) => Some(Err(BridgeRpcError {
                    code: "AccountSuspended".into(),
                    message: "Account is suspended".into(),
                })),
                Err(eve_db::DbError::ValidationError(msg)) => Some(Err(BridgeRpcError {
                    code: "SessionKicked".into(),
                    message: msg,
                })),
                Err(_) => Some(Ok(PyRep::Bool(false))),
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
                    message: "slash.SlashCmd requires GM/Admin role (ROLE_GMS/ROLE_GML/ROLE_GMH/ROLE_ADMIN/ROLE_WORLDMOD)".into(),
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
                let Some(target) = parts.next().map(str::trim).filter(|s| !s.is_empty()) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /ban <account_id> [duration_seconds] [reason]".into(),
                    }));
                };
                const PERMANENT_BAN_UNTIL_UNIX: u64 = 32_503_680_000; // Year 3000 UTC
                let (is_permanent, until_unix, reason): (bool, u64, String) =
                    match parts.next().map(str::trim) {
                        Some(dur_str) => {
                            if let Ok(secs) = dur_str.parse::<u64>() {
                                let r = parts
                                    .next()
                                    .map(str::trim)
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or("Banned by GM")
                                    .to_string();
                                if secs == 0 {
                                    (true, PERMANENT_BAN_UNTIL_UNIX, r)
                                } else {
                                    (false, ctx.now_unix().saturating_add(secs), r)
                                }
                            } else if dur_str.eq_ignore_ascii_case("perm")
                                || dur_str.eq_ignore_ascii_case("permanent")
                                || dur_str.eq_ignore_ascii_case("forever")
                            {
                                let r = parts
                                    .next()
                                    .map(str::trim)
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or("Permanently banned by GM")
                                    .to_string();
                                (true, PERMANENT_BAN_UNTIL_UNIX, r)
                            } else {
                                let full_reason = if let Some(r3) = parts.next() {
                                    format!("{dur_str} {}", r3.trim())
                                } else {
                                    dur_str.to_string()
                                };
                                (true, PERMANENT_BAN_UNTIL_UNIX, full_reason)
                            }
                        }
                        None => (
                            true,
                            PERMANENT_BAN_UNTIL_UNIX,
                            "Banned by GM".to_string(),
                        ),
                    };
                let target_uid = ctx.db.with_snapshot(|s| {
                    if let Ok(id) = target.trim().parse::<u64>() {
                        s.accounts
                            .values()
                            .find(|a| a.user_id == id)
                            .map(|a| a.user_id)
                            .or_else(|| s.accounts.get(target.trim()).map(|a| a.user_id))
                    } else {
                        s.accounts.get(target.trim()).map(|a| a.user_id)
                    }
                });
                let live_sess = target_uid.and_then(|uid| ctx.db.get_online_session(uid));
                match ctx.db.ban_account_by_id_or_username(target, until_unix, &reason) {
                    Ok(uid) => {
                        let username = ctx.db.with_snapshot(|s| {
                            s.accounts
                                .values()
                                .find(|a| a.user_id == uid)
                                .map(|a| a.username.clone())
                                .unwrap_or_else(|| target.to_string())
                        });
                        ctx.release_user_bindings(uid);
                        if let Some(ref s) = live_sess {
                            ctx.disconnect_session(
                                s.session_id,
                                "UserBanned",
                                &format!("Account banned until unix={until_unix}: {reason}"),
                            );
                            let _ = ctx.db.release_online_lock(uid, s.session_id);
                        }
                        Some(Ok(PyRep::keyval([
                            ("command", PyRep::String(cmd.to_string())),
                            ("username", PyRep::String(username)),
                            ("userID", PyRep::Integer(uid as i64)),
                            ("bannedUntilUnix", PyRep::Integer(until_unix as i64)),
                            ("permanent", PyRep::Bool(is_permanent)),
                            ("reason", PyRep::String(reason)),
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
                let mut parts = rest.trim().splitn(2, ' ');
                let target = parts.next().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("");
                if target.is_empty() {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /unban <username|user_id> [reason]".into(),
                    }));
                }
                match ctx.db.unban_account_by_id_or_username(target) {
                    Ok((uid, uname)) => Some(Ok(PyRep::keyval([
                        ("command", PyRep::String(cmd.to_string())),
                        ("username", PyRep::String(uname)),
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
                let Some(target) = parts.next().map(str::trim).filter(|s| !s.is_empty()) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /suspend <username|user_id> <duration_seconds> [reason]".into(),
                    }));
                };
                let Some(dur_str) = parts.next().map(str::trim) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /suspend <username|user_id> <duration_seconds> [reason]".into(),
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
                let target_uid = ctx.db.with_snapshot(|s| {
                    if let Ok(id) = target.trim().parse::<u64>() {
                        s.accounts
                            .values()
                            .find(|a| a.user_id == id)
                            .map(|a| a.user_id)
                            .or_else(|| s.accounts.get(target.trim()).map(|a| a.user_id))
                    } else {
                        s.accounts.get(target.trim()).map(|a| a.user_id)
                    }
                });
                let live_sess = target_uid.and_then(|uid| ctx.db.get_online_session(uid));
                match ctx.db.suspend_account_by_id_or_username(target, until_unix, reason) {
                    Ok(uid) => {
                        let username = ctx.db.with_snapshot(|s| {
                            s.accounts
                                .values()
                                .find(|a| a.user_id == uid)
                                .map(|a| a.username.clone())
                                .unwrap_or_else(|| target.to_string())
                        });
                        ctx.release_user_bindings(uid);
                        if let Some(ref s) = live_sess {
                            ctx.disconnect_session(
                                s.session_id,
                                "AccountSuspended",
                                &format!("Account suspended until unix={until_unix}: {reason}"),
                            );
                            let _ = ctx.db.release_online_lock(uid, s.session_id);
                        }
                        Some(Ok(PyRep::keyval([
                            ("command", PyRep::String(cmd.to_string())),
                            ("username", PyRep::String(username)),
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
                let mut parts = rest.trim().splitn(2, ' ');
                let target = parts.next().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("");
                if target.is_empty() {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /unsuspend <username|user_id> [reason]".into(),
                    }));
                }
                match ctx.db.unsuspend_account_by_id_or_username(target) {
                    Ok((uid, uname)) => Some(Ok(PyRep::keyval([
                        ("command", PyRep::String(cmd.to_string())),
                        ("username", PyRep::String(uname)),
                        ("userID", PyRep::Integer(uid as i64)),
                        ("unsuspended", PyRep::Bool(true)),
                    ]))),
                    Err(e) => Some(Err(BridgeRpcError {
                        code: "UnsuspendFailed".into(),
                        message: e.to_string(),
                    })),
                }
            } else if let Some(rest) = cmd.strip_prefix("/role ") {
                let mut parts = rest.trim().split_whitespace();
                let Some(target) = parts.next() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /role <username|user_id> <role_bitmask_or_name>".into(),
                    }));
                };
                let Some(role_arg) = parts.next() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /role <username|user_id> <role_bitmask_or_name>".into(),
                    }));
                };
                let parsed_role = if let Some(hex_str) = role_arg.strip_prefix("0x").or_else(|| role_arg.strip_prefix("0X")) {
                    u64::from_str_radix(hex_str, 16).ok()
                } else if let Ok(v) = role_arg.parse::<u64>() {
                    Some(v)
                } else {
                    match role_arg.to_ascii_uppercase().as_str() {
                        "PLAYER" | "ROLE_PLAYER" | "ROLE_PLAYER_DEFAULT" => {
                            Some(eve_db::service_roles::ROLE_PLAYER_DEFAULT)
                        }
                        "GMS" | "ROLE_GMS" => Some(
                            eve_db::service_roles::ROLE_PLAYER_DEFAULT
                                | eve_db::service_roles::ROLE_GMS,
                        ),
                        "GML" | "ROLE_GML" => Some(
                            eve_db::service_roles::ROLE_PLAYER_DEFAULT
                                | eve_db::service_roles::ROLE_GML,
                        ),
                        "GMH" | "ROLE_GMH" => Some(
                            eve_db::service_roles::ROLE_PLAYER_DEFAULT
                                | eve_db::service_roles::ROLE_GMH,
                        ),
                        "WORLDMOD" | "ROLE_WORLDMOD" => Some(
                            eve_db::service_roles::ROLE_PLAYER_DEFAULT
                                | eve_db::service_roles::ROLE_WORLDMOD,
                        ),
                        "CONTENT" | "ROLE_CONTENT" => Some(
                            eve_db::service_roles::ROLE_PLAYER_DEFAULT
                                | eve_db::service_roles::ROLE_CONTENT,
                        ),
                        "ADMIN" | "ROLE_ADMIN" | "GM" | "ROLE_GM_DEFAULT" => {
                            Some(eve_db::service_roles::ROLE_GM_DEFAULT)
                        }
                        _ => None,
                    }
                };
                let Some(new_role) = parsed_role else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: format!("Invalid role specification '{role_arg}'"),
                    }));
                };
                match ctx.db.set_account_role(target, new_role) {
                    Ok((uid, uname, old_role)) => {
                        if session.user_id == Some(uid) {
                            session.role = new_role;
                        }
                        let sess_change = PyRep::Dict(vec![(
                            PyRep::String("role".into()),
                            PyRep::Tuple(vec![
                                PyRep::Integer(old_role as i64),
                                PyRep::Integer(new_role as i64),
                            ]),
                        )]);
                        if let Some(online) = ctx.db.get_online_session(uid) {
                            if let Some(target_cid) = online.character_id {
                                ctx.notify(
                                    crate::router::NotificationTarget::Character(target_cid),
                                    "OnSessionChanged",
                                    "charid",
                                    vec![
                                        PyRep::Bool(false),
                                        PyRep::String("role".into()),
                                        sess_change.clone(),
                                    ],
                                );
                            }
                        }
                        Some(Ok(PyRep::keyval([
                            ("command", PyRep::String(cmd.to_string())),
                            ("username", PyRep::String(uname)),
                            ("userID", PyRep::Integer(uid as i64)),
                            ("oldRole", PyRep::Integer(old_role as i64)),
                            ("role", PyRep::Integer(new_role as i64)),
                            ("sessionChange", sess_change),
                        ])))
                    }
                    Err(e) => Some(Err(BridgeRpcError {
                        code: "RoleUpdateFailed".into(),
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
            } else if let Some(rest) = cmd.strip_prefix("/giveitem ").or_else(|| cmd.strip_prefix("/createitem ")) {
                let mut parts = rest.trim().split_whitespace();
                let Some(type_id_str) = parts.next() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /giveitem <type_id> [quantity] or /createitem <type_id> <quantity>".into(),
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
            } else if let Some(rest) = cmd.strip_prefix("/setskill ").or_else(|| cmd.strip_prefix("/giveskill ")) {
                let mut parts = rest.trim().split_whitespace();
                let Some(skill_id_str) = parts.next() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /setskill <skill_id> <level> or /giveskill <skill_id> <level>".into(),
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
            } else if let Some(rest) = cmd.strip_prefix("/teleport ").or_else(|| cmd.strip_prefix("/tr ")) {
                let sys_id_str = rest.trim();
                let Ok(dest_sys_id) = sys_id_str.parse::<u32>() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /teleport <solar_system_id> or /tr <solar_system_id>".into(),
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
            } else if cmd == "/heal" || cmd.starts_with("/heal ") {
                let (sys_id, ship_id) = ctx.db.with_snapshot(|snap| {
                    snap.characters.get(&cid).map(|c| (c.solar_system_id, c.active_ship_id))
                }).unwrap_or((30000142, 0));

                if ship_id > 0 {
                    let handle = ctx.systems.get_or_spawn(sys_id);
                    let _ = handle.heal_combat_state(ship_id).await;
                }

                Some(Ok(PyRep::keyval([
                    ("command", PyRep::String(cmd.to_string())),
                    ("characterID", PyRep::Integer(cid)),
                    ("shipID", PyRep::Integer(ship_id)),
                    ("healed", PyRep::Bool(true)),
                ])))
            } else if let Some(rest) = cmd.strip_prefix("/createccpevent ") {
                if (session.role & eve_machonet::service_roles::ROLE_GMH) == 0 {
                    return Some(Err(BridgeRpcError {
                        code: "InsufficientRole".into(),
                        message: "ROLE_GMH required for /createccpevent".into(),
                    }));
                }
                let mut parts = rest.trim().splitn(4, char::is_whitespace);
                let Some(dt) = parts.next().and_then(|s| s.parse::<i64>().ok()) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /createccpevent <dateTime> <duration> <importance> <title> [| <description>]".into(),
                    }));
                };
                let Some(dur) = parts.next().and_then(|s| s.parse::<i64>().ok()) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /createccpevent <dateTime> <duration> <importance> <title> [| <description>]".into(),
                    }));
                };
                let Some(imp) = parts.next().and_then(|s| s.parse::<i64>().ok()) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /createccpevent <dateTime> <duration> <importance> <title> [| <description>]".into(),
                    }));
                };
                let rem = parts.next().unwrap_or("").trim();
                let (title, desc) = rem.split_once('|').map(|(t, d)| (t.trim(), d.trim())).unwrap_or((rem, ""));
                Some(super::fleet_social::create_ccp_calendar_event(ctx, cid, dt, dur, imp, title, desc))
            } else {
                Some(Err(BridgeRpcError {
                    code: "UnknownSlashCommand".into(),
                    message: format!("Unknown or unsupported slash command '{cmd}'"),
                }))
            }
        }

        _ => super::config_service::handle(ctx, session, service, method, args, kwargs),
    }
}
