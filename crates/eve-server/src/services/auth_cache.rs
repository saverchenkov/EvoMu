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

const ALL_REMOTE_SLASH_COMMANDS: &[&str] = &[
    "achievements",
    "advancetime",
    "autotidi",
    "ban",
    "campaign",
    "chatban",
    "chatunban",
    "cleartimers",
    "completeindustry",
    "completeskills",
    "contact",
    "createccpevent",
    "createitem",
    "despawn",
    "downtime",
    "energy",
    "fastcheckout",
    "fastforward",
    "ff",
    "finishjobs",
    "finishskills",
    "finishstructure",
    "fit",
    "fwcapture",
    "getattr",
    "giveisk",
    "giveitem",
    "giveskill",
    "heal",
    "industrytimer",
    "kick",
    "kill",
    "load",
    "location",
    "moveme",
    "online",
    "operation",
    "pd",
    "reinforce",
    "role",
    "season",
    "secstatus",
    "setattr",
    "setomega",
    "setskill",
    "setunderage",
    "shutdown",
    "spawn",
    "spawnanomaly",
    "spawnnpc",
    "standing",
    "structuretimer",
    "survey",
    "suspend",
    "teleport",
    "tidi",
    "tr",
    "trainskills",
    "translocate",
    "unban",
    "unspawn",
    "unsuspend",
    "upwelltimer",
    "whoami",
];

pub fn remote_slash_commands_for_role(role: u64) -> Vec<&'static str> {
    ALL_REMOTE_SLASH_COMMANDS
        .iter()
        .copied()
        .filter(|cmd| {
            let with_slash = format!("/{cmd}");
            (role & required_slash_role(&with_slash)) != 0
        })
        .collect()
}

/// Formats the `UserError('SlashError', {'reason': ...})` payload expected by
/// `eve/devtools/script/svc_slash.py` `FetchRemoteCommandList` when querying `slash.SlashCmd("/")`.
pub fn format_remote_command_list_reason(role: u64) -> String {
    let quoted: Vec<String> = remote_slash_commands_for_role(role)
        .into_iter()
        .map(|c| format!("'{c}'"))
        .collect();
    format!("Commands: [{}]", quoted.join(", "))
}

pub fn required_slash_role(cmd: &str) -> u64 {
    use eve_machonet::service_roles::{
        ROLE_ADMIN, ROLE_GMH, ROLE_GMS, ROLE_GM_MASK, ROLE_QA, ROLE_ROLEADMIN, ROLE_SPAWN,
    };
    let tok = cmd.split_whitespace().next().unwrap_or("");
    match tok {
        "/" => ROLE_QA | ROLE_SPAWN | ROLE_ROLEADMIN | ROLE_GM_MASK,
        "/online" | "/location" | "/getattr" | "/survey" | "/moveme" | "/translocate"
        | "/whoami" => {
            ROLE_QA | ROLE_GM_MASK
        }
        "/spawn" | "/unspawn" | "/despawn" | "/spawnnpc" => ROLE_SPAWN | ROLE_GM_MASK,
        "/createccpevent" | "/role" => ROLE_GMH | ROLE_GMS | ROLE_ADMIN | ROLE_ROLEADMIN,
        "/advancetime" | "/fastforward" | "/ff" | "/completeskills" | "/trainskills"
        | "/finishskills" | "/upwelltimer" | "/structuretimer" | "/finishstructure"
        | "/completeindustry" | "/finishjobs" | "/industrytimer" | "/cleartimers" => ROLE_GM_MASK,
        _ => ROLE_GM_MASK,
    }
}

pub fn record_gm_audit(ctx: &ServerContext, char_id: i64, cmd: &str, ok: bool) {
    let ts = ctx.now_unix();
    tracing::info!(char_id, cmd, ts, ok, "GM slash command audit");
    if let Err(err) = ctx.db.try_with_mut_snapshot(|snap| {
        let logs = &mut snap.cluster_and_runtime.gm_audit_log;
        logs.push(serde_json::json!({
            "char_id": char_id,
            "cmd": cmd,
            "ts": ts,
            "ok": ok,
        }));
        if logs.len() > 256 {
            let drain_n = logs.len() - 256;
            logs.drain(0..drain_n);
        }
        Ok::<(), String>(())
    }) {
        tracing::warn!(char_id, cmd, "record_gm_audit persistence failed: {err}");
    }
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

        ("machoNet", "NoBlock") => handle_machonet_noblock(ctx, session),

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
                        client_error: None,
                    }));
                }
            };
            let Some(rec) = ctx.get_or_populate_cached_object(object_id, shared) else {
                return Some(Err(BridgeRpcError {
                    code: "CachedObjectNotFound".into(),
                    message: format!("Cached object {:?} is not registered", object_id),
                    client_error: None,
                }));
            };
            Some(Ok(rec.to_cached_object_pyrep()))
        }

        ("machoNet", "ForwardCharacterNotification") => {
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "machoNet.ForwardCharacterNotification requires GM privileges".into(),
                    client_error: None,
                }));
            }
            let Some(char_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "ForwardCharacterNotification requires charID as first argument".into(),
                    client_error: None,
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
                    client_error: None,
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

        ("machoNet", "SetGlobalConfigValue") => {
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "SetGlobalConfigValue requires GM role".into(),
                    client_error: None,
                }));
            }
            let key = args
                .first()
                .and_then(|a| a.as_str())
                .or_else(|| kwargs.get("key").and_then(|a| a.as_str()))
                .unwrap_or("")
                .to_string();
            if key.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "SetGlobalConfigValue requires config key".into(),
                    client_error: None,
                }));
            }
            let val = args
                .get(1)
                .cloned()
                .or_else(|| kwargs.get("value").cloned())
                .or_else(|| kwargs.get("val").cloned())
                .unwrap_or(PyRep::None);
            let config_map = {
                let mut guard = ctx
                    .global_config_values
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                guard.insert(key.clone(), val.clone());
                guard.clone()
            };
            ctx.db.with_mut_snapshot(|snap| {
                snap.cluster_and_runtime.macho_global_config = config_map;
            });
            crate::services::notify_emitters_character_services::emit_global_config_updated(ctx, &key, &val);
            Some(Ok(PyRep::Bool(true)))
        }

        ("cache", "GetServerVersionAndBuild") => {
            // V24 `eve/client/script/ui/services/bugReporting.pyj:218` (`CollectData`):
            // `serverVersion, serverBuild = sm.RemoteSvc('cache').GetServerVersionAndBuild()`
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Real(V24_01_VERSION),
                PyRep::Integer(V24_01_BUILD),
            ])))
        }

        ("cache", "Rowset") => {
            let table_name = args.first().and_then(|a| a.as_str()).unwrap_or("roles");
            let rowset = match table_name {
                "corpmgt__roles" | "roles" => {
                    let rows = crate::domain::corp_governance::CORP_ROLE_DEFINITIONS
                        .iter()
                        .map(|&(bit, client_name, _)| {
                            vec![PyRep::Integer(bit as i64), PyRep::String(client_name.into())]
                        })
                        .collect();
                    PyRep::Rowset { columns: vec!["roleID".into(), "roleName".into()], rows }
                }
                "races" => {
                    let mut ids: Vec<u32> = ctx.sde.races.keys().copied().collect();
                    ids.sort_unstable();
                    let rows = ids.into_iter().filter_map(|id| ctx.sde.races.get(&id).map(|r| vec![PyRep::Integer(i64::from(r.race_id)), PyRep::String(r.name.clone())])).collect();
                    PyRep::Rowset { columns: vec!["raceID".into(), "raceName".into()], rows }
                }
                "factions" => {
                    let mut ids: Vec<u32> = ctx.sde.factions.keys().copied().collect();
                    ids.sort_unstable();
                    let rows = ids.into_iter().filter_map(|id| ctx.sde.factions.get(&id).map(|f| vec![PyRep::Integer(i64::from(f.faction_id)), PyRep::String(f.name.clone())])).collect();
                    PyRep::Rowset { columns: vec!["factionID".into(), "factionName".into()], rows }
                }
                "bloodlines" => {
                    let mut ids: Vec<u32> = ctx.sde.bloodlines.keys().copied().collect();
                    ids.sort_unstable();
                    let rows = ids.into_iter().filter_map(|id| ctx.sde.bloodlines.get(&id).map(|b| vec![PyRep::Integer(i64::from(b.bloodline_id)), PyRep::String(b.name.clone()), PyRep::Integer(i64::from(b.race_id))])).collect();
                    PyRep::Rowset { columns: vec!["bloodlineID".into(), "bloodlineName".into(), "raceID".into()], rows }
                }
                "ancestries" => {
                    let mut ids: Vec<u32> = ctx.sde.ancestries.keys().copied().collect();
                    ids.sort_unstable();
                    let rows = ids.into_iter().filter_map(|id| ctx.sde.ancestries.get(&id).map(|a| vec![PyRep::Integer(i64::from(a.ancestry_id)), PyRep::String(a.name.clone()), PyRep::Integer(i64::from(a.bloodline_id))])).collect();
                    PyRep::Rowset { columns: vec!["ancestryID".into(), "ancestryName".into(), "bloodlineID".into()], rows }
                }
                _ => return Some(Err(BridgeRpcError::user_error("UnknownCacheRowset"))),
            };
            Some(Ok(rowset))
        }

        ("cache", "Setting") => {
            let (section, key, new_val, clear_val) = match args {
                [PyRep::String(s), PyRep::String(k), PyRep::String(v), ..]
                    if !s.is_empty() && !k.is_empty() =>
                {
                    (s.as_str(), k.as_str(), Some(v.as_str()), false)
                }
                [PyRep::String(s), PyRep::String(k), PyRep::None, ..]
                    if !s.is_empty() && !k.is_empty() =>
                {
                    (s.as_str(), k.as_str(), None, true)
                }
                [PyRep::String(s), PyRep::String(k)] if !s.is_empty() && !k.is_empty() => {
                    (s.as_str(), k.as_str(), None, false)
                }
                [PyRep::String(k)] if !k.is_empty() => ("zsystem", k.as_str(), None, false),
                _ => return Some(Err(BridgeRpcError::user_error("InvalidArguments"))),
            };
            let sub_key = format!("{section}:{key}");
            if clear_val {
                if let Err(e) = ServerContext::require_auth(session) {
                    return Some(Err(e));
                }
                if !session.has_gm_role() && (session.role & eve_db::service_roles::ROLE_ADMIN) == 0 {
                    return Some(Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: "NotAuthorized".into(),
                        client_error: None,
                    }));
                }
                let blob_key = format!("cache_setting:{section}:{key}");
                ctx.db.with_mut_snapshot(|snap| {
                    snap.service_state_blobs.remove(&blob_key);
                    snap.kv_mut(eve_db::Ns::CacheSetting).remove(0, &sub_key);
                });
                return Some(Ok(PyRep::None));
            }
            if let Some(v) = new_val {
                if let Err(e) = ServerContext::require_auth(session) {
                    return Some(Err(e));
                }
                if !session.has_gm_role() && (session.role & eve_db::service_roles::ROLE_ADMIN) == 0 {
                    return Some(Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: "NotAuthorized".into(),
                        client_error: None,
                    }));
                }
                let blob_key = format!("cache_setting:{section}:{key}");
                let safe_val: String = v.chars().take(1000).collect();
                let now_unix = ctx.now_unix();
                ctx.db.with_mut_snapshot(|snap| {
                    snap.service_state_blobs.remove(&blob_key);
                    let _ = snap
                        .kv_mut(eve_db::Ns::CacheSetting)
                        .set_with_unix(0, &sub_key, &safe_val, now_unix);
                });
                return Some(Ok(PyRep::String(safe_val)));
            }
            // Read-only path: do not insert arbitrary caller keys into entity_kv / service_state_blobs
            let blob_key = format!("cache_setting:{section}:{key}");
            let val = ctx.db.with_snapshot(|snap| {
                if let Some(existing) = snap
                    .kv(eve_db::Ns::CacheSetting)
                    .get::<String>(0, &sub_key)
                    .or_else(|| snap.service_state_blobs.get(&blob_key).cloned())
                {
                    existing
                } else if key == "SP-Allowed-Extensions" {
                    "txt,csv,log,png,jpg,dds,json,yaml".to_string()
                } else {
                    String::new()
                }
            });
            Some(Ok(PyRep::String(val)))
        }

        ("authentication", "Login") => {
            if args.len() != 2 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidLoginArguments".into(),
                    message: "authentication.Login expects (username, password)".into(),
                    client_error: None,
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
                        client_error: None,
                    });
            let acc = match res {
                Ok(a) => a,
                Err(e) => return Some(Err(e)),
            };

            if (ctx.is_shutting_down()
                || crate::services::serverops_cluster::is_downtime_active_in_db(ctx))
                && (acc.role & eve_machonet::service_roles::ROLE_GMS == 0)
            {
                return Some(Err(BridgeRpcError {
                    code: "ClusterShuttingDown".into(),
                    message: "Cluster shutdown in progress. Non-GM logins are rejected.".into(),
                    client_error: None,
                }));
            }

            if let Some(prev_cid) = session.character_id {
                ctx.logoff_character_sync(prev_cid, Some(session));
            }
            if let Some(prev_uid) = session.user_id {
                if prev_uid != acc.user_id {
                    ctx.db.release_online_lock(prev_uid, session.session_id);
                }
            }
            ctx.release_session_bindings(session.session_id);
            ctx.quasar_gateway.unsubscribe_session(session.session_id);
            ctx.db
                .clear_revocation_on_fresh_login(acc.user_id, session.session_id);

            let lock_res = ctx.db.acquire_online_lock(acc.user_id, session.session_id, None, true, now_unix);
            ctx.db.set_online_session_peer_ip(acc.user_id, session.session_id, &session.address);
            if let Ok(Some(displaced)) = lock_res {
                if let Some(disp_cid) = displaced.character_id {
                    ctx.logoff_character_sync(disp_cid, None);
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
            crate::router::clear_session_character_state(session);
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
                    client_error: None,
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
            let raw_hash = if args.len() == 1 {
                args.first()
            } else if args.is_empty() {
                kwargs
                    .get("computerhash")
                    .or_else(|| kwargs.get("computerHash"))
            } else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidComputerHash".into(),
                    message: "multiLoginBlocker.Login expects a single computerhash argument".into(),
                    client_error: None,
                }));
            };
            // V24 client `characterSelection.pyj` (`_CanLogin`) passes `computerhash` as `args[0]` (str/bytes)
            // and expects a boolean `True` (allowed) or `False` (blocked -> opens `MultiLoginBlockedWindow`).
            let computer_hash = match raw_hash {
                Some(PyRep::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
                Some(PyRep::Bytes(b)) => {
                    let s = String::from_utf8_lossy(b);
                    if s.trim().is_empty() {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidComputerHash".into(),
                            message:
                                "multiLoginBlocker.Login requires a non-empty computerhash string"
                                    .into(),
                            client_error: None,
                        }));
                    }
                    s.trim().to_string()
                }
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidComputerHash".into(),
                        message: "multiLoginBlocker.Login requires a non-empty computerhash string"
                            .into(),
                        client_error: None,
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
                Ok(displaced) => {
                    ctx.db.set_online_session_peer_ip(uid, session.session_id, &session.address);
                    if let Some(prev) = displaced {
                        if prev.session_id != session.session_id {
                            if let Some(prev_cid) = prev.character_id {
                                ctx.logoff_character_sync(prev_cid, None);
                            }
                            ctx.disconnect_session(
                                prev.session_id,
                                "ACL_DISPLACED",
                                "Displaced by concurrent login on same account",
                            );
                            ctx.release_session_bindings(prev.session_id);
                        }
                    }
                    Some(Ok(PyRep::Bool(true)))
                }
                Err(eve_db::DbError::AccountBanned { .. }) => Some(Err(BridgeRpcError {
                    code: "UserBanned".into(),
                    message: "Account is banned".into(),
                    client_error: None,
                })),
                Err(eve_db::DbError::AccountSuspended { .. }) => Some(Err(BridgeRpcError {
                    code: "AccountSuspended".into(),
                    message: "Account is suspended".into(),
                    client_error: None,
                })),
                Err(eve_db::DbError::ValidationError(msg)) => Some(Err(BridgeRpcError {
                    code: "SessionKicked".into(),
                    message: msg,
                    client_error: None,
                })),
                Err(_) => Some(Ok(PyRep::Bool(false))),
            }
        }

        ("subscriptionMgr", "GetCloneGrade") => {
            let uid = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            // Client `clonegrade.const`: `CLONE_STATE_ALPHA = 0`, `CLONE_STATE_OMEGA = 1`
            // (`eve-db` stores `1 = Alpha, 2 = Omega`).
            let clone_grade_u8 = session
                .character_id
                .and_then(|cid| ctx.db.get_character(cid).ok())
                .map(|c| c.clone_grade)
                .unwrap_or_else(|| {
                    ctx.db
                        .get_characters_for_user(uid)
                        .into_iter()
                        .map(|c| c.clone_grade)
                        .max()
                        .unwrap_or(1)
                });
            let client_clone_state = if clone_grade_u8 >= 2 { 1 } else { 0 };
            Some(Ok(PyRep::Integer(client_clone_state)))
        }

        ("loginRewardFacilities", "get_my_clone_grade") => {
            let uid = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let clone_grade_u8 = session
                .character_id
                .and_then(|cid| ctx.db.get_character(cid).ok())
                .map(|c| c.clone_grade)
                .unwrap_or_else(|| {
                    ctx.db
                        .get_characters_for_user(uid)
                        .into_iter()
                        .map(|c| c.clone_grade)
                        .max()
                        .unwrap_or(1)
                });
            let client_clone_state = if clone_grade_u8 >= 2 { 1 } else { 0 };
            Some(Ok(PyRep::Integer(client_clone_state)))
        }

        ("slash", "SlashCmd") => {
            let cmd = args.first().and_then(|a| a.as_str()).unwrap_or("").trim();
            if cmd == "/" {
                if let Err(e) = ServerContext::require_auth(session) {
                    return Some(Err(e));
                }
                return Some(Err(BridgeRpcError {
                    code: "SlashError".into(),
                    message: format_remote_command_list_reason(session.role),
                    client_error: None,
                }));
            }
            let (_, cid) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let req_role = required_slash_role(cmd);
            if (session.role & req_role) == 0 {
                record_gm_audit(ctx, cid, cmd, false);
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: format!("slash.SlashCmd '{cmd}' requires role mask 0x{req_role:x}"),
                    client_error: None,
                }));
            }
            let res: Option<Result<PyRep, BridgeRpcError>> = async {
                if let Some(rest) = cmd.strip_prefix("/giveisk ") {
                let tokens = crate::services::character_skills::character_gm_tools::parse_slash_tokens(rest);
                let Ok(amount) = tokens.first().map(String::as_str).unwrap_or("").parse::<i64>() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /giveisk <positive_integer_amount>".into(),
                        client_error: None,
                    }));
                };
                if amount <= 0 {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Amount for /giveisk must be positive".into(),
                        client_error: None,
                    }));
                }
                let new_bal = ctx.db.try_with_mut_snapshot(|snap| {
                    let ch = snap.characters.get_mut(&cid).ok_or_else(|| BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {cid} not found"),
                        client_error: None,
                    })?;
                    let add = IskCents::from_isk(amount);
                    ch.balance = ch.balance.checked_add(add).ok_or_else(|| BridgeRpcError {
                        code: "BalanceOverflow".into(),
                        message: "Adding ISK would overflow balance".into(),
                        client_error: None,
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
                        client_error: None,
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
                            let released = ctx.db.release_online_lock(uid, s.session_id);
                            ctx.disconnect_session(
                                s.session_id,
                                "UserBanned",
                                &format!("Account banned until unix={until_unix}: {reason}"),
                            );
                            if released {
                                if let Some(banned_cid) = s.character_id {
                                    ctx.logoff_character_sync(banned_cid, None);
                                }
                            }
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
                        client_error: None,
                    })),
                }
            } else if let Some(rest) = cmd.strip_prefix("/unban ") {
                let mut parts = rest.trim().splitn(2, ' ');
                let target = parts.next().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("");
                if target.is_empty() {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /unban <username|user_id> [reason]".into(),
                        client_error: None,
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
                        client_error: None,
                    })),
                }
            } else if let Some(rest) = cmd.strip_prefix("/suspend ") {
                let mut parts = rest.trim().splitn(3, ' ');
                let Some(target) = parts.next().map(str::trim).filter(|s| !s.is_empty()) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /suspend <username|user_id> <duration_seconds> [reason]".into(),
                        client_error: None,
                    }));
                };
                let Some(dur_str) = parts.next().map(str::trim) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /suspend <username|user_id> <duration_seconds> [reason]".into(),
                        client_error: None,
                    }));
                };
                let Ok(duration_secs) = dur_str.parse::<u64>() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Duration for /suspend must be a positive integer in seconds".into(),
                        client_error: None,
                    }));
                };
                if duration_secs == 0 {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Duration for /suspend must be greater than zero".into(),
                        client_error: None,
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
                            let released = ctx.db.release_online_lock(uid, s.session_id);
                            ctx.disconnect_session(
                                s.session_id,
                                "AccountSuspended",
                                &format!("Account suspended until unix={until_unix}: {reason}"),
                            );
                            if released {
                                if let Some(susp_cid) = s.character_id {
                                    ctx.logoff_character_sync(susp_cid, None);
                                }
                            }
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
                        client_error: None,
                    })),
                }
            } else if let Some(rest) = cmd.strip_prefix("/unsuspend ") {
                let mut parts = rest.trim().splitn(2, ' ');
                let target = parts.next().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("");
                if target.is_empty() {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /unsuspend <username|user_id> [reason]".into(),
                        client_error: None,
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
                        client_error: None,
                    })),
                }
            } else if let Some(rest) = cmd.strip_prefix("/role ") {
                let mut parts = rest.trim().split_whitespace();
                let Some(target) = parts.next() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /role <username|user_id> <role_bitmask_or_name>".into(),
                        client_error: None,
                    }));
                };
                let Some(role_arg) = parts.next() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /role <username|user_id> <role_bitmask_or_name>".into(),
                        client_error: None,
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
                        client_error: None,
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
                        client_error: None,
                    })),
                }
            } else if let Some(rest) = cmd.strip_prefix("/kick ") {
                let mut parts = rest.trim().splitn(2, ' ');
                let Some(username) = parts.next().map(str::trim).filter(|s| !s.is_empty()) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /kick <username> [reason]".into(),
                        client_error: None,
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
                        client_error: None,
                    }));
                };
                if is_admin {
                    return Some(Err(BridgeRpcError {
                        code: "KickFailed".into(),
                        message: format!("Cannot kick administrator account '{username}'"),
                        client_error: None,
                    }));
                }
                let kicked_sess = match ctx.db.try_kick_user(uid, reason, ctx.now_unix()) {
                    Ok(sess) => sess,
                    Err(e) => {
                        return Some(Err(BridgeRpcError {
                            code: "KickFailed".into(),
                            message: e.to_string(),
                            client_error: None,
                        }));
                    }
                };
                ctx.release_user_bindings(uid);
                if let Some(ref s) = kicked_sess {
                    ctx.disconnect_session(s.session_id, "SessionKicked", reason);
                    if let Some(kicked_cid) = s.character_id {
                        ctx.logoff_character_sync(kicked_cid, None);
                    }
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
                let tokens = crate::services::character_skills::character_gm_tools::parse_slash_tokens(rest);
                let Some(type_id_str) = tokens.first().map(String::as_str) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /giveitem <type_id> [quantity] or /createitem <type_id> <quantity>".into(),
                        client_error: None,
                    }));
                };
                let Ok(type_id) = type_id_str.parse::<u32>() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Invalid type_id for /giveitem".into(),
                        client_error: None,
                    }));
                };
                let qty: i32 = tokens.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
                if qty <= 0 {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Quantity for /giveitem must be positive".into(),
                        client_error: None,
                    }));
                }
                let Some(inv_type) = ctx.sde.types.get(&type_id) else {
                    return Some(Err(BridgeRpcError {
                        code: "TypeNotFound".into(),
                        message: format!("Type ID {type_id} not found in SDE"),
                        client_error: None,
                    }));
                };
                let is_singleton = inv_type.category_id == 6
                    || (qty == 1 && matches!(inv_type.category_id, 7 | 32));
                let res = ctx.db.try_with_mut_snapshot(|snap| {
                    let ch = snap.characters.get(&cid).ok_or_else(|| BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {cid} not found"),
                        client_error: None,
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
                        singleton: is_singleton,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    };
                    snap.items.insert(item_id, item.clone());
                    Ok((item, location_id))
                });
                match res {
                    Ok((item, loc_id)) => {
                        let item_id = item.item_id;
                        // Issue #988: route newly created items through `notify_item_mutation`
                        // so `OnItemChange` and blueprint `OnBlueprintsUpdated(ownerID)` stay consistent.
                        ctx.notify_item_mutation(cid, None, Some(&item));
                        Some(Ok(PyRep::keyval([
                            ("command", PyRep::String(cmd.to_string())),
                            ("itemID", PyRep::Integer(item_id)),
                            ("typeID", PyRep::Integer(type_id as i64)),
                            ("quantity", PyRep::Integer(qty as i64)),
                            ("locationID", PyRep::Integer(loc_id)),
                        ])))
                    }
                    Err(e) => Some(Err(e)),
                }
            } else if let Some(rest) = cmd.strip_prefix("/setskill ").or_else(|| cmd.strip_prefix("/giveskill ")) {
                let tokens = crate::services::character_skills::character_gm_tools::parse_slash_tokens(rest);
                let token_strs: Vec<&str> = tokens.iter().map(String::as_str).collect();
                let (skill_id_opt, lvl_opt) = match token_strs.as_slice() {
                    [s, l] => (Some(*s), Some(*l)),
                    [_target, s, l, ..] => (Some(*s), Some(*l)),
                    [s] => (Some(*s), None),
                    [] => (None, None),
                };
                let Some(skill_id_str) = skill_id_opt else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /setskill <skill_id> <level> or /giveskill <skill_id> <level>".into(),
                        client_error: None,
                    }));
                };
                let Ok(skill_id) = skill_id_str.parse::<u32>() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Invalid skill_id for /setskill".into(),
                        client_error: None,
                    }));
                };
                let Some(lvl_str) = lvl_opt else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /setskill <skill_id> <level>".into(),
                        client_error: None,
                    }));
                };
                let Ok(lvl) = lvl_str.parse::<u8>() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Invalid level for /setskill".into(),
                        client_error: None,
                    }));
                };
                if lvl > 5 {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Level for /setskill must be between 0 and 5".into(),
                        client_error: None,
                    }));
                }
                let Some(skill_type) = ctx.sde.types.get(&skill_id) else {
                    return Some(Err(BridgeRpcError {
                        code: "SkillNotFound".into(),
                        message: format!("Skill type ID {skill_id} not found in SDE"),
                        client_error: None,
                    }));
                };
                if skill_type.category_id != 16 {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSkillType".into(),
                        message: format!("Type {skill_id} is not a skill (category 16)"),
                        client_error: None,
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
                        client_error: None,
                    })?;
                    let skills_map = snap.skills.entry(cid).or_default();
                    let prev_sp = skills_map.get(&skill_id).map(|s| s.skill_points).unwrap_or(0.0);
                    let prev_lvl = skills_map.get(&skill_id).map(|s| s.level).unwrap_or(0);
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
                    Ok((target_sp, ch.skill_points, active_ship, prev_lvl))
                });
                match res {
                    Ok((sp, total_sp, active_ship, prev_lvl)) => {
                        ctx.invalidate_ship_fitting(active_ship);
                        if !session.address.is_empty() {
                            if lvl == 0 {
                                crate::services::jump_clone_names::emit_server_skills_removed(
                                    ctx,
                                    cid,
                                    &[(skill_id, prev_lvl)],
                                );
                            } else {
                                crate::services::jump_clone_names::emit_server_skills_changed(
                                    ctx,
                                    cid,
                                    &[skill_id],
                                    Some("GMGiveSkill"),
                                );
                            }
                        }
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
                        client_error: None,
                    }));
                };
                if !ctx.sde.types.contains_key(&type_id) {
                    return Some(Err(BridgeRpcError {
                        code: "TypeNotFound".into(),
                        message: format!("NPC type ID {type_id} not found in SDE"),
                        client_error: None,
                    }));
                }
                let (sys_id, ship_id) = ctx.db.with_snapshot(|snap| {
                    snap.characters.get(&cid).map(|c| (c.solar_system_id, c.active_ship_id))
                }).or_else(|| session.solar_system_id.map(|s| (s, 0))).unwrap_or((0, 0));

                let handle = ctx.systems.get_or_spawn(sys_id);
                let origin = if ship_id > 0 {
                    handle.get_ball_snapshot(ship_id).await.ok().flatten().map(|(_, b)| b.position).unwrap_or(DVec3::ZERO)
                } else {
                    DVec3::ZERO
                };
                let npc_id = crate::services::combat_crimewatch::combat_npc_ai::spawn_npc_entity_from_sde(
                    ctx,
                    sys_id,
                    type_id,
                    0,
                    origin + DVec3::new(1_000.0, 0.0, 0.0),
                )
                .await;

                Some(Ok(PyRep::keyval([
                    ("command", PyRep::String(cmd.to_string())),
                    ("npcID", PyRep::Integer(npc_id)),
                    ("typeID", PyRep::Integer(type_id as i64)),
                    ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ])))
            } else if let Some(rest) = ["/teleport", "/tr", "/translocate", "/moveme"]
                .iter()
                .find_map(|prefix| {
                    if cmd == *prefix {
                        Some("")
                    } else {
                        cmd.strip_prefix(prefix)
                            .filter(|r| r.starts_with(char::is_whitespace))
                    }
                })
            {
                Some(handle_slash_teleport(ctx, session, cid, cmd, rest).await)
            } else if cmd == "/heal" || cmd.starts_with("/heal ") {
                Some(
                    crate::services::character_skills::character_gm_tools::execute_gm_heal(
                        ctx, session, cid, cmd,
                    )
                    .await,
                )
            } else if let Some(rest) = cmd.strip_prefix("/createccpevent ") {
                if (session.role & eve_machonet::service_roles::ROLE_GMH) == 0 {
                    return Some(Err(BridgeRpcError {
                        code: "InsufficientRole".into(),
                        message: "ROLE_GMH required for /createccpevent".into(),
                        client_error: None,
                    }));
                }
                let mut parts = rest.trim().splitn(4, char::is_whitespace);
                let Some(dt) = parts.next().and_then(|s| s.parse::<i64>().ok()) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /createccpevent <dateTime> <duration> <importance> <title> [| <description>]".into(),
                        client_error: None,
                    }));
                };
                let Some(dur) = parts.next().and_then(|s| s.parse::<i64>().ok()) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /createccpevent <dateTime> <duration> <importance> <title> [| <description>]".into(),
                        client_error: None,
                    }));
                };
                let Some(imp) = parts.next().and_then(|s| s.parse::<i64>().ok()) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /createccpevent <dateTime> <duration> <importance> <title> [| <description>]".into(),
                        client_error: None,
                    }));
                };
                let rem = parts.next().unwrap_or("").trim();
                let (title, desc) = rem.split_once('|').map(|(t, d)| (t.trim(), d.trim())).unwrap_or((rem, ""));
                Some(super::fleet_social::create_ccp_calendar_event(ctx, cid, dt, dur, imp, title, desc))
            } else {
                Some(Err(BridgeRpcError {
                    code: "UnknownSlashCommand".into(),
                    message: format!("Unknown or unsupported slash command '{cmd}'"),
                    client_error: None,
                }))
            }
            }
            .await;
            if let Some(ref r) = res {
                record_gm_audit(ctx, cid, cmd, r.is_ok());
            }
            res
        }

        _ => super::config_service::handle(ctx, session, service, method, args, kwargs),
    }
}

/// Handles `/teleport`, `/tr`, `/translocate`, and `/moveme` slash commands.
///
/// Matches real V24.01 client slash syntax emitted by `menusvc.pyj`, `svc_slash.pyj`, and
/// `publicQaToolsClient.pyj` (`[me|<char_id>] <location_id> [noblock]`, as well as
/// `<location_id> me [noblock]`), resolving destinations across solar systems, NPC stations,
/// Upwell structures, stargates, SDE celestials, and live in-system balls.
async fn handle_slash_teleport(
    ctx: &ServerContext,
    session: &mut SessionState,
    cid: i64,
    cmd: &str,
    rest: &str,
) -> Result<PyRep, BridgeRpcError> {
    let tokens = crate::services::character_skills::character_gm_tools::parse_slash_tokens(rest);
    let args: Vec<&str> = tokens
        .iter()
        .map(String::as_str)
        .filter(|t| !t.eq_ignore_ascii_case("noblock"))
        .collect();

    let (target_cid, loc_str) = match args.as_slice() {
        [loc] => (cid, *loc),
        [who, loc] if who.eq_ignore_ascii_case("me") || who.eq_ignore_ascii_case("myship") => {
            (cid, *loc)
        }
        [loc, who] if who.eq_ignore_ascii_case("me") || who.eq_ignore_ascii_case("myship") => {
            (cid, *loc)
        }
        [who, loc] => {
            let Ok(parsed_cid) = who.parse::<i64>() else {
                return Err(BridgeRpcError {
                    code: "InvalidSlashArgument".into(),
                    message: "Usage: /teleport|/tr|/translocate|/moveme [me|<char_id>] <location_id>"
                        .into(),
                    client_error: None,
                });
            };
            if parsed_cid != cid
                && !ctx.db.with_snapshot(|s| s.characters.contains_key(&parsed_cid))
            {
                return Err(BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: format!("Character {parsed_cid} not found"),
                    client_error: None,
                });
            }
            (parsed_cid, *loc)
        }
        _ => {
            return Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Usage: /teleport|/tr|/translocate|/moveme [me|<char_id>] <location_id>"
                    .into(),
                client_error: None,
            });
        }
    };

    let Ok(target_id) = loc_str.parse::<i64>() else {
        return Err(BridgeRpcError {
            code: "InvalidSlashArgument".into(),
            message: "Usage: /teleport|/tr|/translocate|/moveme [me|<char_id>] <location_id>"
                .into(),
            client_error: None,
        });
    };
    if target_id <= 0 {
        return Err(BridgeRpcError {
            code: "InvalidSlashArgument".into(),
            message: "Usage: /teleport|/tr|/translocate|/moveme [me|<char_id>] <location_id>"
                .into(),
            client_error: None,
        });
    }

    let (old_sys_id, ship_id, was_in_space) = ctx
        .db
        .with_snapshot(|snap| {
            snap.characters.get(&target_cid).map(|c| {
                (
                    c.solar_system_id,
                    c.active_ship_id,
                    c.station_id.is_none() && c.structure_id.is_none(),
                )
            })
        })
        .or_else(|| {
            if target_cid == cid {
                session.solar_system_id.map(|s| {
                    (
                        s,
                        0,
                        session.station_id.is_none() && session.structure_id.is_none(),
                    )
                })
            } else {
                None
            }
        })
        .unwrap_or((0, 0, false));

    let (dest_sys_id, explicit_pos): (u32, Option<DVec3>) = if let Some(sys_id) =
        u32::try_from(target_id)
            .ok()
            .filter(|sid| ctx.sde.solar_systems.contains_key(sid))
    {
        (sys_id, None)
    } else if let Some(st) = ctx.sde.stations.get(&target_id) {
        (st.solar_system_id, Some(st.position + DVec3::new(5_000.0, 0.0, 0.0)))
    } else if let Some(structure) =
        ctx.db.with_snapshot(|snap| snap.structures.get(&target_id).cloned())
    {
        if structure.state == "Destroyed" {
            return Err(BridgeRpcError {
                code: "StructureDestroyed".into(),
                message: format!("Structure {target_id} is destroyed"),
                client_error: None,
            });
        }
        (
            structure.solar_system_id,
            Some(DVec3::from_array(structure.position) + DVec3::new(5_000.0, 0.0, 0.0)),
        )
    } else if let Some(jump) = ctx.sde.jumps.get(&target_id) {
        (
            jump.from_solar_system_id,
            Some(jump.position + DVec3::new(2_500.0, 0.0, 0.0)),
        )
    } else if let Some(cel_sys_id) = ctx.sde.location_solar_system(target_id) {
        let cel_pos = ctx
            .sde
            .celestials_in_system(cel_sys_id)
            .into_iter()
            .find(|c| c.celestial_id == target_id)
            .map(|c| c.position + DVec3::new(c.radius.max(1_000.0) + 1_500.0, 0.0, 0.0));
        (cel_sys_id, cel_pos)
    } else if old_sys_id > 0 {
        let sys_handle = ctx.systems.get_or_spawn(old_sys_id);
        if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(target_id).await {
            (
                old_sys_id,
                Some(ball.position + DVec3::new(ball.radius.max(500.0) + 1_500.0, 0.0, 0.0)),
            )
        } else {
            return Err(BridgeRpcError {
                code: "SolarSystemNotFound".into(),
                message: format!("Solar system or location {target_id} not found"),
                client_error: None,
            });
        }
    } else {
        return Err(BridgeRpcError {
            code: "SolarSystemNotFound".into(),
            message: format!("Solar system or location {target_id} not found"),
            client_error: None,
        });
    };

    let updated_ch = ctx
        .db
        .set_character_solar_system(target_cid, dest_sys_id, &ctx.sde)
        .map_err(|e| BridgeRpcError {
            code: "TeleportFailed".into(),
            message: e.to_string(),
            client_error: None,
        })?;

    if ship_id > 0 {
        let default_spawn_pos = || {
            if !session.address.is_empty() {
                ctx.sde
                    .stations
                    .get(&60_003_760)
                    .filter(|s| s.solar_system_id == dest_sys_id)
                    .or_else(|| {
                        ctx.sde
                            .stations
                            .values()
                            .find(|s| s.solar_system_id == dest_sys_id)
                    })
                    .map_or(DVec3::new(2_500_000_000.0, 0.0, 0.0), |s| {
                        s.position + DVec3::new(1_500.0, 0.0, 0.0)
                    })
            } else {
                DVec3::new(10_000.0, 0.0, 0.0)
            }
        };

        if was_in_space && old_sys_id > 0 {
            let removed_active = ctx
                .active_warps
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&ship_id)
                .is_some();
            let removed_return = ctx
                .return_warps
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&ship_id);
            let pre_handle = ctx.systems.get_or_spawn(old_sys_id);
            let ball_warping = pre_handle
                .get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
                .is_some_and(|(_, b)| matches!(b.mode, MovementMode::Warp { .. }));
            if removed_active || removed_return || ball_warping {
                // #994: Emit and flush Stop + effects.Warping(0) in a preceding DoDestinyUpdate
                // before SetState / SessionChangeNotification tears down the client shipModel.
                let _ = pre_handle.force_stop_warp(ship_id);
                let _ = pre_handle.flush_peer_observer_updates(None).await;
            }
        }

        let persisted_pos;
        if old_sys_id != dest_sys_id {
            let existing_ball = if old_sys_id > 0 {
                let old_handle = ctx.systems.get_or_spawn(old_sys_id);
                if was_in_space {
                    crate::services::space_navigation_advanced::deactivate_ship_modules_on_leave_space(
                        ctx, target_cid, ship_id,
                    );
                }
                let taken = old_handle.take_ball(ship_id).await;
                if let Some(ref t) = taken {
                    ctx.db.with_mut_snapshot(|snap| {
                        crate::sp6_lifecycle::leave_space_in_snap(
                            snap,
                            ship_id,
                            t.combat_state.as_ref(),
                            Some(dest_sys_id),
                            None,
                        );
                    });
                }
                taken.map(|t| t.ball)
            } else {
                None
            };
            if old_sys_id > 0 {
                ctx.sync_character_local_chat(target_cid, Some(old_sys_id), dest_sys_id);
            }
            let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
            let tr_pos = explicit_pos.unwrap_or_else(default_spawn_pos);
            let ship_tid = ctx
                .db
                .with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.type_id))
                .unwrap_or(587);
            let mut new_ball = existing_ball.unwrap_or(Ball {
                id: ship_id,
                type_id: ship_tid,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            });
            new_ball.position = tr_pos;
            new_ball.velocity = DVec3::ZERO;
            new_ball.speed_fraction = 0.0;
            new_ball.mode = MovementMode::Stop;
            let combat_state = ctx.build_ship_combat_state(target_cid, ship_id, new_ball.type_id);
            let slim_meta = ctx.build_ship_slim_meta(target_cid, new_ball.type_id);
            let _ = new_handle
                .spawn_ball_with_meta(new_ball, combat_state, slim_meta)
                .await;
            let _ = new_handle.push_set_state(ship_id).await;
            persisted_pos = Some(tr_pos);
        } else {
            let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
            let existing_ball = new_handle
                .get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b);
            if existing_ball.is_none() {
                let tr_pos = explicit_pos.unwrap_or_else(default_spawn_pos);
                let ship_tid = ctx
                    .db
                    .with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.type_id))
                    .unwrap_or(587);
                let new_ball = Ball {
                    id: ship_id,
                    type_id: ship_tid,
                    position: tr_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 0.5,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                };
                let combat_state = ctx.build_ship_combat_state(target_cid, ship_id, ship_tid);
                let slim_meta = ctx.build_ship_slim_meta(target_cid, ship_tid);
                let _ = new_handle
                    .spawn_ball_with_meta(new_ball, combat_state, slim_meta)
                    .await;
                let _ = new_handle.push_set_state(ship_id).await;
                persisted_pos = Some(tr_pos);
            } else {
                if let Some(tr_pos) = explicit_pos {
                    let _ = new_handle.set_ball_position(ship_id, tr_pos).await;
                }
                let _ = new_handle
                    .set_movement_mode(ship_id, MovementMode::Stop)
                    .await;
                let _ = new_handle.push_set_state(ship_id).await;
                persisted_pos = explicit_pos;
            }
        }

        if let Some(pos) = persisted_pos {
            ctx.db.with_mut_snapshot(|snap| {
                snap.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
            });
        }
    }

    let sess_target = if target_cid == cid {
        Some(session)
    } else {
        None
    };
    let teleport_reason = if was_in_space && old_sys_id == dest_sys_id {
        "same_system_teleport"
    } else {
        "slash_teleport"
    };
    let _ = ctx.execute_session_transition_sync(
        sess_target,
        crate::router::SessionTransitionSpec::for_kind(
            target_cid,
            crate::router::TransitionKind::Teleport {
                solar_system_id: dest_sys_id,
                dock_id: None,
                ship_id: None,
            },
            teleport_reason,
        )
        .with_apply_db(false)
        .with_sync_local_chat(false)
        .with_emit_on_session_changed(false),
    );

    Ok(PyRep::keyval([
        ("command", PyRep::String(cmd.to_string())),
        ("characterID", PyRep::Integer(target_cid)),
        ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
        (
            "constellationID",
            PyRep::Integer(updated_ch.constellation_id as i64),
        ),
        ("regionID", PyRep::Integer(updated_ch.region_id as i64)),
    ]))
}

/// Handles `machoNet.NoBlock` client keepalive ping semantics.
///
/// The client periodically issues non-blocking pings (`machoNet.NoBlock`) to confirm transport
/// liveness and refresh proxy/gateway session activity without mutating game state.
#[inline]
fn handle_machonet_noblock(
    _ctx: &ServerContext,
    _session: &mut SessionState,
) -> Option<Result<PyRep, BridgeRpcError>> {
    Some(Ok(PyRep::Bool(true)))
}

#[cfg(test)] #[path = "auth_cache_tests.rs"] mod auth_cache_tests;

