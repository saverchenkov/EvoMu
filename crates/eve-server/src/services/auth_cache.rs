//! `auth_cache` — Domain 1: Session, Authentication, Object Caching, Config, & Quasar Gateway (`§3.1`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::IskCents;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;
const V24_01_VERSION: f64 = 24.01;
const V24_01_BUILD: i64 = 3_619_879;

pub fn handle(
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
            let allow_displace = args.first().and_then(|a| a.as_bool()).unwrap_or(false);
            match ctx.db.acquire_online_lock(
                uid,
                session.session_id,
                session.character_id,
                allow_displace,
                ctx.now_unix(),
            ) {
                Ok(displaced) => {
                    if let Some(ref d) = displaced {
                        ctx.release_session_bindings(d.session_id);
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
                    Ok(bal) => Some(Ok(PyRep::keyval([
                        ("command", PyRep::String(cmd.to_string())),
                        ("newBalanceISK", PyRep::Real(bal)),
                    ]))),
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
                match ctx.db.ban_account(username, until_unix, reason) {
                    Ok(uid) => {
                        ctx.release_user_bindings(uid);
                        Some(Ok(PyRep::keyval([
                            ("command", PyRep::String(cmd.to_string())),
                            ("username", PyRep::String(username.to_string())),
                            ("userID", PyRep::Integer(uid as i64)),
                            ("bannedUntilUnix", PyRep::Integer(until_unix as i64)),
                            ("reason", PyRep::String(reason.to_string())),
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
            } else {
                Some(Err(BridgeRpcError {
                    code: "UnknownSlashCommand".into(),
                    message: format!("Unknown or unsupported slash command '{cmd}'"),
                }))
            }
        }

        _ => None,
    }
}
