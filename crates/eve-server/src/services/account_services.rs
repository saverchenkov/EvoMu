//! `account_services` — account-level services split out of `auth_cache`: playtime accounting
//! (`authentication.AccruedTime/AmUnderage`, `userSvc.GetSessionPlaytimeMinutes`), redeem tokens
//! (`userSvc.GetRedeemTokens/ClaimRedeemTokens/TrashRedeemTokens/ReverseRedeem`, GM-only
//! `CreateRedeemTokenQA`), and Kiring (China) paths reported disabled for Tranquility.
//!
//! Note: `petitioner.*` handlers have been modularly extracted to `crate::services::petitioner_ops`.
//!
//! Self-contained (own side store) so it can be hooked into `auth_cache::handle` with one line.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub use crate::services::petitioner_ops::{
    default_petition_queue_id, PetitionEntry, CANONICAL_PETITION_CATEGORIES, CANONICAL_PETITION_QUEUES,
};

/// Private JSON side-store key in `item_custom_names`.
pub const ACCOUNT_SERVICES_STORE_KEY: i64 = -10_000_004;

pub(crate) fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.to_string(),
        message: message.into(),
    }
}

pub(crate) fn req_gm(session: &SessionState, ctx_name: &str) -> Result<(), BridgeRpcError> {
    if session.has_gm_role() {
        Ok(())
    } else {
        Err(err(
            "InsufficientRole",
            format!("{ctx_name} requires GM/Admin role"),
        ))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedeemTokenEntry {
    pub token_id: i64,
    pub type_id: u32,
    pub quantity: i32,
    pub description: String,
    pub station_id: Option<i64>,
    #[serde(default)]
    pub expire_date_time: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountServicesStore {
    pub next_id: i64,
    #[serde(default)]
    pub next_message_id: i64,
    #[serde(default)]
    pub redeem_tokens: HashMap<u64, Vec<RedeemTokenEntry>>,
    #[serde(default)]
    pub login_start_unix: HashMap<u64, u64>,
    #[serde(default)]
    pub accrued_playtime_secs: HashMap<u64, u64>,
    #[serde(default)]
    pub underage_users: HashSet<u64>,
    #[serde(default)]
    pub petitions: HashMap<i64, PetitionEntry>,
    /// userID -> Omega subscription end (unix seconds). Set only by GM tooling, never by the client.
    #[serde(default)]
    pub subscription_expiry: HashMap<u64, u64>,
    #[serde(default)]
    pub mct_slots: HashMap<u64, Vec<u64>>,
    #[serde(default)]
    pub votes: HashMap<(u64, String), i64>,
    #[serde(default)]
    pub resculpt_pending: HashSet<i64>,
    #[serde(default)]
    pub account_create_date: HashMap<u64, i64>,
    #[serde(default)]
    pub user_countries: HashMap<u64, String>,
    #[serde(default)]
    pub zendesk_disabled: bool,
    #[serde(default)]
    pub disabled_categories: HashSet<i64>,
}

impl AccountServicesStore {
    pub fn alloc_id(&mut self) -> i64 {
        if self.next_id < 600_000 { self.next_id = 600_000; }
        self.next_id += 1;
        self.next_id
    }
    pub fn alloc_message_id(&mut self) -> i64 {
        self.next_message_id += 1;
        self.next_message_id
    }
}

pub fn load_store(snap: &eve_db::WorldStateSnapshot) -> AccountServicesStore {
    snap.item_custom_names
        .get(&ACCOUNT_SERVICES_STORE_KEY)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

pub fn enqueue_redeem_token_in_snapshot(
    snap: &mut eve_db::WorldStateSnapshot,
    uid: u64,
    type_id: u32,
    quantity: i32,
    description: String,
    station_id: Option<i64>,
    expire_date_time: Option<i64>,
) -> i64 {
    let mut store = load_store(snap);
    let id = store.alloc_id();
    store
        .redeem_tokens
        .entry(uid)
        .or_default()
        .push(RedeemTokenEntry {
            token_id: id,
            type_id,
            quantity,
            description,
            station_id,
            expire_date_time,
        });
    if let Ok(json) = serde_json::to_string(&store) {
        snap.item_custom_names.insert(ACCOUNT_SERVICES_STORE_KEY, json);
    }
    id
}

pub fn with_store<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut eve_db::WorldStateSnapshot, &mut AccountServicesStore) -> R,
) -> R {
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let out = f(snap, &mut store);
        if let Ok(json) = serde_json::to_string(&store) {
            snap.item_custom_names.insert(ACCOUNT_SERVICES_STORE_KEY, json);
        }
        out
    })
}

pub(crate) fn try_with_store<T>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut eve_db::WorldStateSnapshot, &mut AccountServicesStore) -> Result<T, BridgeRpcError>,
) -> Result<T, BridgeRpcError> {
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let out = f(snap, &mut store)?;
        if let Ok(json) = serde_json::to_string(&store) {
            snap.item_custom_names.insert(ACCOUNT_SERVICES_STORE_KEY, json);
        }
        Ok(out)
    })
}

/// Called by `authentication.Login`: starts the playtime accrual window for the account.
pub fn on_login(ctx: &ServerContext, uid: u64) {
    let now = ctx.now_unix();
    with_store(ctx, |_, store| {
        store.login_start_unix.insert(uid, now);
        store.user_countries.entry(uid).or_insert_with(|| "US".into());
    });
}

pub fn set_user_country(ctx: &ServerContext, uid: u64, country: impl Into<String>) {
    with_store(ctx, |_, store| {
        store.user_countries.insert(uid, country.into());
    });
}

/// Called by `authentication.Logout`: folds the session's playtime into the account total.
pub fn on_logout(ctx: &ServerContext, uid: u64) {
    let now = ctx.now_unix();
    with_store(ctx, |_, store| {
        if let Some(start) = store.login_start_unix.remove(&uid) {
            *store.accrued_playtime_secs.entry(uid).or_insert(0) += now.saturating_sub(start);
        }
    });
}

/// Reads the recorded Omega subscription expiry timestamp for `uid` from `snap`, if any.
pub fn subscription_expiry_for_user(snap: &eve_db::WorldStateSnapshot, uid: u64) -> Option<u64> {
    let admin_exp = load_store(snap).subscription_expiry.get(&uid).copied();
    let quasar_exp = snap
        .owner_notes
        .get(&-8_500_000_000_000_i64)
        .and_then(|n| {
            serde_json::from_str::<crate::services::quasar_auth_user::QuasarIdentityStore>(&n.note)
                .ok()
        })
        .and_then(|st| st.omega_subscriptions.get(&uid).copied());
    match (admin_exp, quasar_exp) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Applies the stored Omega expiry to every character on the account: Omega (2) while the
/// expiry is in the future, Alpha (1) once it has passed. Accounts with no recorded expiry keep
/// their characters' current clone grade. Returns `(grade, expiry_unix)`.
pub fn sync_subscription(ctx: &ServerContext, uid: u64) -> (u32, u64) {
    let now = ctx.now_unix();
    let cids_to_catch_up: Vec<i64> = ctx.db.with_snapshot(|snap| {
        let lapsed = subscription_expiry_for_user(snap, uid).is_some_and(|until| until <= now);
        if lapsed {
            snap.characters
                .values()
                .filter(|c| c.user_id == uid && c.clone_grade >= 2)
                .map(|c| c.character_id)
                .collect()
        } else {
            Vec::new()
        }
    });
    for cid in cids_to_catch_up {
        crate::services::character_skills::catch_up_skill_queue(ctx, cid);
    }
    let (res_grade, res_exp, changed_grades, exp_changed) = with_store(ctx, |snap, store| {
        let admin_exp = store.subscription_expiry.get(&uid).copied();
        let quasar_exp = snap
            .owner_notes
            .get(&-8_500_000_000_000_i64)
            .and_then(|n| {
                serde_json::from_str::<crate::services::quasar_auth_user::QuasarIdentityStore>(
                    &n.note,
                )
                .ok()
            })
            .and_then(|st| st.omega_subscriptions.get(&uid).copied());
        let exp = match (admin_exp, quasar_exp) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        let exp_changed = exp.is_some() && exp != admin_exp;
        if let Some(u) = exp {
            store.subscription_expiry.insert(uid, u);
        }
        let target = exp.map(|until| if until > now { 2u32 } else { 1u32 });
        let mut grade = None;
        let mut changed = Vec::new();
        for ch in snap.characters.values_mut().filter(|c| c.user_id == uid) {
            if let Some(g) = target {
                if ch.clone_grade != g {
                    ch.clone_grade = g;
                    ch.version += 1;
                    changed.push((ch.character_id, g));
                }
            }
            grade.get_or_insert(ch.clone_grade);
        }
        (target.or(grade).unwrap_or(1), exp.unwrap_or(0), changed, exp_changed)
    });
    if exp_changed {
        crate::services::notify_emitters_batch9::emit_subscription_time_update_server(ctx, uid, res_exp);
    }
    for (cid, g) in changed_grades {
        crate::services::notify_emitters_batch9::emit_subscription_changed_server(ctx, cid, g);
    }
    (res_grade, res_exp)
}

/// GM tooling (`/setomega <userID> <unixExpiry>`): records the account's Omega expiry and applies it.
pub fn set_subscription_expiry(ctx: &ServerContext, uid: u64, until: u64) -> (u32, u64) {
    with_store(ctx, |_, store| {
        store.subscription_expiry.insert(uid, until);
    });
    crate::services::notify_emitters_batch9::emit_subscription_time_update_server(ctx, uid, until);
    sync_subscription(ctx, uid)
}

/// Called by `authentication.Logout` while the character is still bound to the session: persists
/// the session's current location on the character and, if the character is in space, removes
/// its ship ball from the system. Returns `(removed_from_space, saved_solar_system_id)`.
pub async fn save_location_and_leave_space(
    ctx: &ServerContext,
    session: &SessionState,
) -> (bool, u32) {
    let Some(cid) = session.character_id else {
        return (false, 0);
    };
    let info = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&cid)?;
        if let Some(sys) = session.solar_system_id {
            ch.solar_system_id = sys;
        }
        ch.station_id = session.station_id;
        ch.structure_id = session.structure_id;
        ch.version += 1;
        Some((
            ch.solar_system_id,
            ch.active_ship_id,
            ch.station_id.is_none() && ch.structure_id.is_none(),
        ))
    });
    let Some((sys_id, ship_id, in_space)) = info else {
        return (false, 0);
    };
    let removed = if in_space && ship_id > 0 {
        let _ = crate::sp6_lifecycle::handle_character_logoff_or_disconnect(ctx, cid).await;
        true
    } else {
        false
    };
    (removed, sys_id)
}

/// GM tooling: flags/unflags an account as underage (reported by `authentication.AmUnderage`).
pub fn set_underage(ctx: &ServerContext, uid: u64, flag: bool) {
    with_store(ctx, |_, store| {
        if flag {
            store.underage_users.insert(uid);
        } else {
            store.underage_users.remove(&uid);
        }
    });
}

/// Returns `Some(result)` for the RPCs owned by this module, `None` otherwise.
pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep], kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if (service, method) == ("authentication", "Logout") {
        let (removed_from_space, saved_sys) = save_location_and_leave_space(ctx, session).await;
        if let Some(cid) = session.character_id {
            // Watchers of this character get OnClientContactChange(charID, False).
            crate::services::character_skills::notify_watchlist_presence(ctx, cid, false);
            crate::services::fleet_social::fleet_social_notify::on_character_logout(ctx, cid);
        }
        if let Some(uid) = session.user_id {
            on_logout(ctx, uid);
            ctx.db.release_online_lock(uid, session.session_id);
        }
        ctx.release_session_bindings(session.session_id);
        session.user_id = None;
        session.role = 0;
        session.character_id = None;
        session.fleet_id = None;
        session.corporation_id = None;
        session.alliance_id = None;
        session.station_id = None;
        session.structure_id = None;
        session.solar_system_id = None;
        session.constellation_id = None;
        session.region_id = None;
        session.ship_id = None;
        return Some(Ok(PyRep::keyval([
            ("loggedOut", PyRep::Bool(true)),
            ("removedFromSpace", PyRep::Bool(removed_from_space)),
            ("savedSolarSystemID", PyRep::Integer(saved_sys as i64)),
        ])));
    }
    if (service, method) == ("userSvc", "UserLogOffCharacter") {
        let (_, cid) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let _ = save_location_and_leave_space(ctx, session).await;
        crate::services::character_skills::notify_watchlist_presence(ctx, cid, false);
        crate::services::fleet_social::fleet_social_notify::on_character_logout(ctx, cid);
        if let Some(uid) = session.user_id {
            let _ = ctx
                .db
                .acquire_online_lock(uid, session.session_id, None, true, ctx.now_unix());
        }
        ctx.release_session_bindings(session.session_id);
        session.character_id = None;
        session.fleet_id = None;
        session.ship_id = None;
        session.station_id = None;
        session.structure_id = None;
        session.solar_system_id = None;
        return Some(Ok(PyRep::Bool(true)));
    }
    if (service, method) == ("multiLoginBlocker", "Login") {
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
            return Some(Err(err(
                "InvalidComputerHash",
                "multiLoginBlocker.Login expects a single computerhash argument",
            )));
        };
        let computer_hash = match raw_hash {
            Some(PyRep::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
            Some(PyRep::Bytes(b)) => {
                let s = String::from_utf8_lossy(b);
                if s.trim().is_empty() {
                    return Some(Err(err(
                        "InvalidComputerHash",
                        "multiLoginBlocker.Login requires a non-empty computerhash string",
                    )));
                }
                s.trim().to_string()
            }
            _ => {
                return Some(Err(err(
                    "InvalidComputerHash",
                    "multiLoginBlocker.Login requires a non-empty computerhash string",
                )))
            }
        };
        if ctx
            .db
            .has_alpha_computer_hash_conflict(uid, session.session_id, &computer_hash)
        {
            return Some(Ok(PyRep::Bool(false)));
        }
        return match ctx.db.acquire_online_lock_with_computer_hash(
            uid,
            session.session_id,
            session.character_id,
            Some(computer_hash),
            false,
            now_unix,
        ) {
            Ok(displaced) => {
                if let Some(prev) = displaced {
                    if prev.session_id != session.session_id {
                        if let Some(prev_cid) = prev.character_id {
                            let _ = crate::sp6_lifecycle::handle_character_logoff_or_disconnect(
                                ctx, prev_cid,
                            )
                            .await;
                            crate::services::character_skills::notify_watchlist_presence(
                                ctx, prev_cid, false,
                            );
                            crate::services::fleet_social::fleet_social_notify::on_character_logout(
                                ctx, prev_cid,
                            );
                            crate::services::fleet_social::fleet_social_notify::handle_character_disconnect(
                                ctx, prev_cid,
                            );
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
            Err(eve_db::DbError::AccountBanned { .. }) => {
                Some(Err(err("UserBanned", "Account is banned")))
            }
            Err(eve_db::DbError::AccountSuspended { .. }) => {
                Some(Err(err("AccountSuspended", "Account is suspended")))
            }
            Err(eve_db::DbError::ValidationError(msg)) => Some(Err(err("SessionKicked", msg))),
            Err(_) => Some(Ok(PyRep::Bool(false))),
        };
    }
    if service == "petitioner" {
        return super::petitioner_ops::handle(ctx, session, service, method, args, kwargs);
    }
    if let Some(res) = super::character_lifecycle::handle(ctx, session, service, method, args, kwargs) {
        return Some(res);
    }
    match handle_inner(ctx, session, service, method, args, kwargs) {
        Ok(Some(rep)) => Some(Ok(rep)),
        Ok(None) => None,
        Err(e) => Some(Err(e)),
    }
}

fn handle_inner(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep], kwargs: &HashMap<String, PyRep>,
) -> Result<Option<PyRep>, BridgeRpcError> {
    match (service, method) {
        ("subscriptionMgr", "GetSubscriptionTime") => {
            // Read-only: arguments are ignored; the client cannot change its own subscription.
            let uid = ServerContext::require_auth(session)?;
            let (grade, exp) = sync_subscription(ctx, uid);
            Ok(Some(PyRep::keyval([
                ("userID", PyRep::Integer(uid as i64)),
                ("subscriptionEndTime", PyRep::Integer(exp as i64)),
                ("cloneGrade", PyRep::Integer(grade as i64)),
                ("isOmega", PyRep::Bool(grade == 2)),
            ])))
        }
        ("subscriptionMgr", "GetCloneGrade") => {
            // Apply any lapsed/renewed expiry first, then let `auth_cache` build the reply.
            let uid = ServerContext::require_auth(session)?;
            sync_subscription(ctx, uid);
            Ok(None)
        }
        ("slash", "SlashCmd") => {
            let cmd = args
                .first()
                .and_then(|a| a.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let is_ours = cmd.starts_with("/setomega ") || cmd.starts_with("/setunderage ");
            if !is_ours {
                return Ok(None);
            }
            let _ = ServerContext::require_character(session)?;
            req_gm(session, "slash.SlashCmd")?;
            let mut parts = cmd.split_whitespace();
            let verb = parts.next().unwrap_or("");
            let uid = parts
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .ok_or_else(|| {
                    err(
                        "InvalidSlashArgument",
                        format!("Usage: {verb} <userID> <value>"),
                    )
                })?;
            if !ctx
                .db
                .with_snapshot(|s| s.accounts.values().any(|a| a.user_id == uid))
            {
                return Err(err("AccountNotFound", format!("Account {uid} not found")));
            }
            let value = parts.next().unwrap_or("");
            if verb == "/setomega" {
                let until = value.parse::<u64>().map_err(|_| {
                    err(
                        "InvalidSlashArgument",
                        "Usage: /setomega <userID> <unixExpiry>",
                    )
                })?;
                let (grade, exp) = set_subscription_expiry(ctx, uid, until);
                Ok(Some(PyRep::keyval([
                    ("userID", PyRep::Integer(uid as i64)),
                    ("cloneGrade", PyRep::Integer(grade as i64)),
                    ("subscriptionEndTime", PyRep::Integer(exp as i64)),
                ])))
            } else {
                let flag = matches!(value, "1" | "true");
                set_underage(ctx, uid, flag);
                Ok(Some(PyRep::keyval([
                    ("userID", PyRep::Integer(uid as i64)),
                    ("underage", PyRep::Bool(flag)),
                ])))
            }
        }
        ("authentication", "AccruedTime") => {
            let uid = ServerContext::require_auth(session)?;
            let now_unix = ctx.now_unix();
            let accrued = with_store(ctx, |_, store| {
                let start = store
                    .login_start_unix
                    .get(&uid)
                    .copied()
                    .unwrap_or(now_unix);
                let base = store.accrued_playtime_secs.get(&uid).copied().unwrap_or(0);
                base.saturating_add(now_unix.saturating_sub(start))
            });
            Ok(Some(PyRep::Integer(accrued as i64)))
        }
        ("authentication", "AmUnderage") => {
            let uid = ServerContext::require_auth(session)?;
            let underage = ctx
                .db
                .with_snapshot(|snap| load_store(snap).underage_users.contains(&uid));
            Ok(Some(PyRep::Bool(underage)))
        }
        ("userSvc", "CreateRedeemTokenQA") => {
            let uid = ServerContext::require_auth(session)?;
            req_gm(session, "userSvc.CreateRedeemTokenQA")?;
            let Some(type_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .filter(|&v| v > 0)
                .map(|v| v as u32)
            else {
                return Err(err("InvalidTypeID", "CreateRedeemTokenQA requires typeID"));
            };
            if !ctx.sde.types.contains_key(&type_id) {
                return Err(err(
                    "InvalidTypeID",
                    format!("Type {type_id} not found in SDE"),
                ));
            }
            let qty = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1).max(1) as i32;
            let desc = args
                .get(2)
                .and_then(|a| a.as_str())
                .unwrap_or("Redeemable Gift")
                .to_string();
            let tok_id = with_store(ctx, |_, store| {
                let id = store.alloc_id();
                store
                    .redeem_tokens
                    .entry(uid)
                    .or_default()
                    .push(RedeemTokenEntry {
                        token_id: id,
                        type_id,
                        quantity: qty,
                        description: desc.clone(),
                        station_id: None,
                        expire_date_time: None,
                    });
                id
            });
            crate::services::notify_emitters_batch9::emit_redeeming_queue_updated_for_user(
                ctx,
                uid,
                session.character_id,
            );
            Ok(Some(PyRep::keyval([
                ("tokenID", PyRep::Integer(tok_id)),
                ("massTokenID", PyRep::Integer(tok_id)),
                ("typeID", PyRep::Integer(type_id as i64)),
                ("quantity", PyRep::Integer(qty as i64)),
                ("description", PyRep::String(desc)),
            ])))
        }
        ("userSvc", "GetRedeemTokens") => {
            let uid = ServerContext::require_auth(session)?;
            let tokens = ctx.db.with_snapshot(|snap| {
                load_store(snap)
                    .redeem_tokens
                    .get(&uid)
                    .cloned()
                    .unwrap_or_default()
            });
            let list: Vec<PyRep> = tokens
                .into_iter()
                .map(|t| {
                    PyRep::keyval([
                        ("tokenID", PyRep::Integer(t.token_id)),
                        ("massTokenID", PyRep::Integer(t.token_id)),
                        ("typeID", PyRep::Integer(t.type_id as i64)),
                        ("quantity", PyRep::Integer(t.quantity as i64)),
                        ("description", PyRep::String(t.description)),
                        (
                            "stationID",
                            t.station_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                        ),
                        (
                            "expireDateTime",
                            t.expire_date_time.map(PyRep::Integer).unwrap_or(PyRep::None),
                        ),
                    ])
                })
                .collect();
            Ok(Some(PyRep::List(list)))
        }
        ("userSvc", "ClaimRedeemTokens") => {
            let uid = ServerContext::require_auth(session)?;
            let mut token_ids = Vec::new();
            let mut target_cid = args
                .get(1)
                .and_then(|v| v.as_i64())
                .or_else(|| {
                    kwargs
                        .get("charID")
                        .or_else(|| kwargs.get("characterID"))
                        .and_then(|v| v.as_i64())
                })
                .or(session.character_id);
            let tokens_arg = args.first().or_else(|| kwargs.get("tokens"));
            match tokens_arg {
                Some(PyRep::List(items) | PyRep::Tuple(items)) => {
                    for it in items {
                        if let Some(id) = it.as_i64() {
                            token_ids.push(id);
                        } else if let PyRep::Tuple(pair) | PyRep::List(pair) = it {
                            if let Some(id) = pair.first().and_then(|p| p.as_i64()) {
                                token_ids.push(id);
                            }
                        } else if let PyRep::Dict(entries) = it {
                            if let Some(id) = entries
                                .iter()
                                .find(|(k, _)| k.as_str() == Some("tokenID"))
                                .and_then(|(_, v)| v.as_i64())
                            {
                                token_ids.push(id);
                            }
                        } else if let PyRep::KeyVal(kv) = it {
                            if let Some(id) = kv.get("tokenID").and_then(|v| v.as_i64()) {
                                token_ids.push(id);
                            }
                        }
                    }
                }
                Some(PyRep::Integer(v)) => {
                    if args.len() >= 2 {
                        token_ids.push(*v);
                    } else if ctx.db.with_snapshot(|s| s.characters.contains_key(v)) {
                        target_cid = Some(*v);
                    } else {
                        token_ids.push(*v);
                    }
                }
                _ => {}
            }
            let Some(cid) = target_cid else {
                return Err(err(
                    "CharacterNotSelected",
                    "ClaimRedeemTokens requires a target characterID",
                ));
            };
            let (claimed, created, loc, added_non_dim_qty) = try_with_store(ctx, |snap, store| {
                let Some(ch) = snap.characters.get(&cid).cloned() else {
                    return Err(err(
                        "CharacterNotFound",
                        format!("Character {cid} not found"),
                    ));
                };
                if ch.user_id != uid {
                    return Err(err(
                        "CharacterOwnershipMismatch",
                        "Cannot redeem tokens to a character on another account",
                    ));
                }
                if token_ids.is_empty() {
                    return Err(err(
                        "RedeemTokenNotFound",
                        "No matching redeem tokens found",
                    ));
                }
                let dest_loc = ch
                    .station_id
                    .or(ch.structure_id)
                    .unwrap_or(ch.home_station_id);
                let list = store.redeem_tokens.entry(uid).or_default();
                let mut claimed_toks = Vec::new();
                let mut created_items = Vec::new();
                let mut added_non_dim_qty: i64 = 0;
                let mut remaining = Vec::new();
                let now_ft = ctx.now_blueos_filetime_ticks();
                for tok in list.drain(..) {
                    if token_ids.contains(&tok.token_id) {
                        if tok.expire_date_time.is_some_and(|exp| now_ft > exp) {
                            remaining.push(tok);
                            continue;
                        }
                        if super::implant_plugging::is_non_diminishing_injector(&ctx.sde, tok.type_id) {
                            added_non_dim_qty += i64::from(tok.quantity.max(0));
                        }
                        snap.next_entity_id += 1;
                        let item_id = snap.next_entity_id;
                        let is_ship = ctx
                            .sde
                            .types
                            .get(&tok.type_id)
                            .is_some_and(|t| t.category_id == 6);
                        snap.items.insert(
                            item_id,
                            InventoryItemRecord {
                                item_id,
                                type_id: tok.type_id,
                                owner_id: cid,
                                location_id: tok.station_id.unwrap_or(dest_loc),
                                flag: InventoryFlag::Hangar,
                                quantity: tok.quantity,
                                singleton: is_ship,
                                version: 1,
                                dynamic_attributes: HashMap::new(),
                                source_mutaplasmid_id: None,
                                insured_payout_isk: 0.0,
                                is_blueprint_copy: false,
                                blueprint_runs_remaining: None,
                            },
                        );
                        claimed_toks.push(PyRep::Integer(tok.token_id));
                        created_items.push(PyRep::Integer(item_id));
                    } else {
                        remaining.push(tok);
                    }
                }
                *list = remaining;
                if claimed_toks.is_empty() {
                    return Err(err(
                        "RedeemTokenNotFound",
                        "No matching redeem tokens found",
                    ));
                }
                Ok((claimed_toks, created_items, dest_loc, added_non_dim_qty))
            })?;
            crate::services::notify_emitters_batch9::emit_redeeming_queue_updated_for_user(
                ctx,
                uid,
                Some(cid),
            );
            if added_non_dim_qty > 0 {
                crate::services::notify_emitters_batch9::emit_non_diminishing_injections_added(
                    ctx,
                    cid,
                    added_non_dim_qty,
                );
            }
            Ok(Some(PyRep::keyval([
                ("claimedTokenIDs", PyRep::List(claimed)),
                ("createdItemIDs", PyRep::List(created)),
                ("characterID", PyRep::Integer(cid)),
                ("stationID", PyRep::Integer(loc)),
            ])))
        }
        ("userSvc", "TrashRedeemTokens") | ("userSvc", "ReverseRedeem") => {
            let uid = ServerContext::require_auth(session)?;
            let mut token_ids = Vec::new();
            if let Some(PyRep::List(items) | PyRep::Tuple(items)) = args.first() {
                for it in items {
                    if let Some(id) = it.as_i64() {
                        token_ids.push(id);
                    } else if let PyRep::Tuple(pair) | PyRep::List(pair) = it {
                        if let Some(id) = pair.first().and_then(|p| p.as_i64()) {
                            token_ids.push(id);
                        }
                    }
                }
            } else if let Some(id) = args.first().and_then(|a| a.as_i64()) {
                token_ids.push(id);
            }
            let removed = with_store(ctx, |_, store| {
                let list = store.redeem_tokens.entry(uid).or_default();
                let before = list.len();
                list.retain(|t| !token_ids.contains(&t.token_id));
                before.saturating_sub(list.len())
            });
            if removed > 0 {
                crate::services::notify_emitters_batch9::emit_redeeming_queue_updated_for_user(
                    ctx,
                    uid,
                    session.character_id,
                );
            }
            Ok(Some(PyRep::Bool(removed > 0)))
        }
        ("userSvc", "GetSessionPlaytimeMinutes") => {
            let uid = ServerContext::require_auth(session)?;
            let now_unix = ctx.now_unix();
            let mins = with_store(ctx, |_, store| {
                let start = store
                    .login_start_unix
                    .get(&uid)
                    .copied()
                    .unwrap_or(now_unix);
                let base = store.accrued_playtime_secs.get(&uid).copied().unwrap_or(0);
                base.saturating_add(now_unix.saturating_sub(start)) / 60
            });
            Ok(Some(PyRep::Integer(mins as i64)))
        }
        ("userSvc", "GetMultiCharactersTrainingSlots") => {
            let uid = ServerContext::require_auth(session)?;
            let slots = with_store(ctx, |snap, store| {
                if let Some(blob) = snap.service_state_blobs.get(&format!("mct_slots:{uid}")) {
                    if let Ok(parsed) = serde_json::from_str::<Vec<u64>>(blob) {
                        return parsed;
                    }
                }
                store.mct_slots.get(&uid).cloned().unwrap_or_default()
            });
            let now_unix = ctx.now_unix();
            let dict: Vec<(PyRep, PyRep)> = slots
                .into_iter()
                .enumerate()
                .map(|(idx, exp)| {
                    (
                        PyRep::Integer(idx as i64 + 1),
                        PyRep::keyval([
                            ("slotID", PyRep::Integer(idx as i64 + 1)),
                            ("expiryDate", PyRep::Integer(exp as i64)),
                            ("active", PyRep::Bool(exp > now_unix)),
                        ]),
                    )
                })
                .collect();
            Ok(Some(PyRep::Dict(dict)))
        }
        ("userSvc", "GetUserName") => {
            let uid = ServerContext::require_auth(session)?;
            let name = ctx.db.with_snapshot(|snap| {
                snap.accounts
                    .values()
                    .find(|a| a.user_id == uid)
                    .map(|a| a.username.clone())
                    .unwrap_or_else(|| "pilot".into())
            });
            Ok(Some(PyRep::String(name)))
        }
        ("userSvc", "GetCreateDate") => {
            let uid = ServerContext::require_auth(session)?;
            let dt = with_store(ctx, |_, store| {
                *store
                    .account_create_date
                    .entry(uid)
                    .or_insert_with(|| ctx.now_blueos_filetime_ticks())
            });
            Ok(Some(PyRep::Integer(dt)))
        }
        ("userSvc", "GetUserToken") => {
            let uid = ServerContext::require_auth(session)?;
            Ok(Some(PyRep::String(format!("sso-tok-{uid}"))))
        }
        ("userSvc", "CastUpvote") | ("userSvc", "CastDownvote") => {
            let uid = ServerContext::require_auth(session)?;
            let target = args.first().and_then(|a| a.as_str()).unwrap_or("general");
            let delta = if method == "CastUpvote" { 1i64 } else { -1i64 };
            with_store(ctx, |_, store| {
                *store.votes.entry((uid, target.to_string())).or_default() += delta;
            });
            Ok(Some(PyRep::Bool(true)))
        }
        ("userSvc", "ReportBot") | ("userSvc", "ReportISKSpammer") => {
            let (_, reporter_id) = ServerContext::require_character(session)?;
            let reported_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let reason = args.get(1).and_then(|a| a.as_str()).unwrap_or("Report");
            let nid = ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.owner_notes.insert(id, eve_db::OwnerNoteRecord {
                    note_id: id,
                    character_id: reporter_id,
                    item_id: reported_id,
                    label: format!("{method}:{reported_id}"),
                    note: reason.to_string(),
                    timestamp_unix: ctx.now_unix(),
                });
                id
            });
            Ok(Some(PyRep::Integer(nid)))
        }
        ("userSvc", "ActivateMultiTraining") | ("userSvc", "ActivateSoulboundMultiTraining") => {
            let uid = ServerContext::require_auth(session)?;
            let (_, char_id) = ServerContext::require_character(session)?;
            let res: Result<(), BridgeRpcError> = ctx.db.try_with_mut_snapshot(|snap| {
                let cert_found = snap.items.values_mut().find(|it| {
                    (it.type_id == 33027 || it.type_id == 34189 || it.type_id == 48582)
                        && it.owner_id == char_id
                        && it.quantity >= 1
                });
                if let Some(cert) = cert_found {
                    cert.quantity -= 1;
                    cert.version += 1;
                    let cert_id = cert.item_id;
                    let qty = cert.quantity;
                    if qty == 0 {
                        snap.items.remove(&cert_id);
                    }
                    Ok(())
                } else {
                    Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: "NoMultiTrainingCertificateFound".into(),
                    })
                }
            });
            res?;
            let now_unix = ctx.now_unix();
            let expiry = now_unix + 30 * 86_400;
            with_store(ctx, |snap, store| {
                let slots = store.mct_slots.entry(uid).or_default();
                slots.push(expiry);
                if let Ok(json) = serde_json::to_string(slots) {
                    snap.service_state_blobs.insert(format!("mct_slots:{uid}"), json);
                }
            });
            crate::services::notify_emitters_batch9::emit_multiple_characters_training_updated(
                ctx,
                uid,
            );
            Ok(Some(PyRep::keyval([
                ("expiryDate", PyRep::Integer(expiry as i64)),
                ("slotActivated", PyRep::Bool(true)),
            ])))
        }
        ("userSvc", "GetUpdatedMultiCharacterTraining") | ("userSvc", "InvalidateMultiCharacterTrainingCache") => {
            let uid = ServerContext::require_auth(session)?;
            let slots = with_store(ctx, |snap, store| {
                if let Some(blob) = snap.service_state_blobs.get(&format!("mct_slots:{uid}")) {
                    if let Ok(parsed) = serde_json::from_str::<Vec<u64>>(blob) {
                        return parsed;
                    }
                }
                store.mct_slots.get(&uid).cloned().unwrap_or_default()
            });
            if method == "InvalidateMultiCharacterTrainingCache" {
                crate::services::notify_emitters_batch9::emit_multiple_characters_training_updated(
                    ctx,
                    uid,
                );
            }
            let now_unix = ctx.now_unix();
            let list = slots.into_iter().map(|exp| PyRep::keyval([
                ("expiryDate", PyRep::Integer(exp as i64)),
                ("active", PyRep::Bool(exp > now_unix)),
            ])).collect();
            Ok(Some(PyRep::List(list)))
        }
        ("userSvc", "ActivateCharacterReSculpt") => {
            let uid = ServerContext::require_auth(session)?;
            let (_, char_id) = ServerContext::require_character(session)?;
            let res: Result<(), BridgeRpcError> = ctx.db.try_with_mut_snapshot(|snap| {
                let cert_found = snap.items.values_mut().find(|it| {
                    (it.type_id == 28662 || it.type_id == 33502) && it.owner_id == char_id && it.quantity >= 1
                });
                if let Some(cert) = cert_found {
                    cert.quantity -= 1;
                    cert.version += 1;
                    let cert_id = cert.item_id;
                    let qty = cert.quantity;
                    if qty == 0 {
                        snap.items.remove(&cert_id);
                    }
                    Ok(())
                } else {
                    Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: "NoResculptCertificateFound".into(),
                    })
                }
            });
            res?;
            with_store(ctx, |snap, store| {
                store.resculpt_pending.insert(char_id);
                let cur: u32 = snap
                    .service_state_blobs
                    .get(&format!("account_resculpt_tokens:{uid}"))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                snap.service_state_blobs.insert(format!("account_resculpt_tokens:{uid}"), (cur + 1).to_string());
                snap.service_state_blobs.insert(format!("resculpt_pending:{char_id}"), "true".to_string());
                let mut map: HashMap<String, u32> = snap
                    .service_state_blobs
                    .get("account_resculpt_tokens")
                    .and_then(|v| serde_json::from_str(v).ok())
                    .unwrap_or_default();
                *map.entry(uid.to_string()).or_default() += 1;
                *map.entry(char_id.to_string()).or_default() += 1;
                if let Ok(j) = serde_json::to_string(&map) {
                    snap.service_state_blobs.insert("account_resculpt_tokens".to_string(), j);
                }
            });
            Ok(Some(PyRep::Bool(true)))
        }
        ("userSvc", "ProcessEVESessionCreated") => {
            let uid = ServerContext::require_auth(session)?;
            let sess_id = session.session_id;
            let char_id = session
                .character_id
                .or_else(|| args.first().and_then(|a| a.as_i64()));
            if let Some(cid) = char_id {
                let belongs = ctx.db.with_snapshot(|snap| {
                    snap.characters.get(&cid).is_some_and(|c| c.user_id == uid)
                });
                if !belongs {
                    return Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: "CharacterDoesNotBelongToUser".into(),
                    });
                }
            }
            let now = ctx.now_unix();
            ctx.db.with_mut_snapshot(|snap| {
                let mut sessions: Vec<serde_json::Value> = snap
                    .service_state_blobs
                    .get("user_sessions")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                sessions.push(serde_json::json!({
                    "user_id": uid,
                    "session_id": sess_id,
                    "character_id": char_id,
                    "timestamp_unix": now,
                }));
                if sessions.len() > 256 {
                    let trim = sessions.len() - 256;
                    sessions.drain(0..trim);
                }
                if let Ok(json) = serde_json::to_string(&sessions) {
                    snap.service_state_blobs.insert("user_sessions".into(), json);
                }
            });
            Ok(Some(PyRep::None))
        }
        ("kiringMgr", "GetKiringConfiguration") => {
            let is_underage = session
                .user_id
                .is_some_and(|uid| with_store(ctx, |_, st| st.underage_users.contains(&uid)));
            Ok(Some(PyRep::keyval([
                ("enabled", PyRep::Bool(false)),
                ("antiAddictionEnabled", PyRep::Bool(is_underage)),
                ("region", PyRep::String("TQ".into())),
            ])))
        }
        ("kiringMgr", "GetAntiAddictionCode") | ("kiringMgr", "GetSAuthAid") => {
            let code = session
                .user_id
                .map(|uid| with_store(ctx, |_, st| if st.underage_users.contains(&uid) { 1 } else { 0 }))
                .unwrap_or(0);
            Ok(Some(PyRep::Integer(code)))
        }
        ("kiringMgr", "GetEveGuardServerAddress") => {
            let _ = session
                .user_id
                .map(|uid| with_store(ctx, |_, st| st.underage_users.contains(&uid)));
            Ok(Some(PyRep::None))
        }
        ("kiringMgr", "IsMonochromeStyleEnabled") | ("kiringMgr", "ShouldKickCheater") => {
            let banned = session
                .user_id
                .is_some_and(|uid| with_store(ctx, |_, st| st.underage_users.contains(&uid)));
            Ok(Some(PyRep::Bool(banned)))
        }
        _ => Ok(None),
    }
}
