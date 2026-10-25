//! `account_services` — account-level services split out of `auth_cache`: playtime accounting
//! (`authentication.AccruedTime/AmUnderage`, `userSvc.GetSessionPlaytimeMinutes`), redeem tokens
//! (`userSvc.GetRedeemTokens/ClaimRedeemTokens/TrashRedeemTokens/ReverseRedeem`, GM-only
//! `CreateRedeemTokenQA`), Kiring (China) paths reported disabled for Tranquility, and petitions
//! (`petitioner.*`).
//!
//! Self-contained (own side store) so it can be hooked into `auth_cache::handle` with one line.

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Private JSON side-store key in `item_custom_names`.
pub(crate) const ACCOUNT_SERVICES_STORE_KEY: i64 = -10_000_004;

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.to_string(),
        message: message.into(),
    }
}

fn req_gm(session: &SessionState, ctx_name: &str) -> Result<(), BridgeRpcError> {
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
}
fn default_petition_queue_id() -> i64 { 1 }

// DERIVED: server-configured support petition categories (id, parent_id, name, description, is_billing) and queues.
const CANONICAL_PETITION_CATEGORIES: &[(i64, i64, &str, &str, bool)] = &[
    (1, 0, "Game Play Support", "General gameplay assistance", false),
    (2, 0, "Billing & Account", "Account and subscription support", true),
    (3, 0, "Technical Issues", "Client and connectivity issues", false),
    (4, 0, "Stuck Character", "Character or ship stuck in space", false),
    (101, 1, "Missions & Exploration", "Mission objectives and deadspace pockets", false),
    (102, 1, "Reimbursement", "Ship and module loss reimbursement requests", false),
    (201, 2, "Omega Subscription", "Subscription time and billing inquiries", true),
    (202, 2, "PLEX & NES", "PLEX vault and New Eden Store purchases", true),
    (301, 3, "Crash & Performance", "Client crash and graphics issues", false),
    (401, 4, "Stuck in Space", "Unable to warp, dock, or jump", false),
];

// DERIVED: server-configured GM support petition queues.
const CANONICAL_PETITION_QUEUES: &[(i64, &str)] = &[
    (1, "Default"),
    (2, "Escalated"),
    (3, "Billing"),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PetitionEntry {
    pub petition_id: i64,
    pub petitioner_cid: i64,
    pub subject: String,
    pub description: String,
    pub category_id: i64,
    #[serde(default = "default_petition_queue_id")]
    pub queue_id: i64,
    pub status: String,
    pub claimed_by: Option<i64>,
    pub escalated: bool,
    #[serde(default)]
    pub unread: bool,
    #[serde(default)]
    pub last_unread_message_id: i64,
    pub rating: Option<i64>,
    #[serde(default)]
    pub response_time_rating: Option<i64>,
    #[serde(default)]
    pub helpfulness_rating: Option<i64>,
    #[serde(default)]
    pub attitude_rating: Option<i64>,
    pub rating_comment: String,
    pub messages: Vec<(i64, String, u64)>,
    pub logs: Vec<(String, i64, u64)>,
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

fn load_store(snap: &eve_db::WorldStateSnapshot) -> AccountServicesStore {
    snap.item_custom_names
        .get(&ACCOUNT_SERVICES_STORE_KEY)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

fn with_store<R>(
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

fn try_with_store<T>(
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
    load_store(snap).subscription_expiry.get(&uid).copied()
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
    with_store(ctx, |snap, store| {
        let exp = store.subscription_expiry.get(&uid).copied();
        let target = exp.map(|until| if until > now { 2u32 } else { 1u32 });
        let mut grade = None;
        for ch in snap.characters.values_mut().filter(|c| c.user_id == uid) {
            if let Some(g) = target {
                if ch.clone_grade != g {
                    ch.clone_grade = g;
                    ch.version += 1;
                }
            }
            grade.get_or_insert(ch.clone_grade);
        }
        (target.or(grade).unwrap_or(1), exp.unwrap_or(0))
    })
}

/// GM tooling (`/setomega <userID> <unixExpiry>`): records the account's Omega expiry and applies it.
pub fn set_subscription_expiry(ctx: &ServerContext, uid: u64, until: u64) -> (u32, u64) {
    with_store(ctx, |_, store| {
        store.subscription_expiry.insert(uid, until);
    });
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
        if args.len() != 1 {
            return Some(Err(err(
                "InvalidComputerHash",
                "multiLoginBlocker.Login expects a single computerhash argument",
            )));
        }
        let computer_hash = match args.first() {
            Some(PyRep::String(s)) if !s.trim().is_empty() => s.clone(),
            Some(PyRep::Bytes(b)) if !b.is_empty() => String::from_utf8_lossy(b).into_owned(),
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
                    ctx.disconnect_session(
                        prev.session_id,
                        "ACL_DISPLACED",
                        "Displaced by concurrent login on same account",
                    );
                    ctx.release_session_bindings(prev.session_id);
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
    args: &[PyRep], _kwargs: &HashMap<String, PyRep>,
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
                    });
                id
            });
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
                    ])
                })
                .collect();
            Ok(Some(PyRep::List(list)))
        }
        ("userSvc", "ClaimRedeemTokens") => {
            let uid = ServerContext::require_auth(session)?;
            let mut token_ids = Vec::new();
            let mut target_cid = session.character_id;
            for arg in args {
                match arg {
                    PyRep::List(items) | PyRep::Tuple(items) => {
                        for it in items {
                            if let Some(id) = it.as_i64() {
                                token_ids.push(id);
                            } else if let PyRep::Tuple(pair) = it {
                                if let Some(id) = pair.first().and_then(|p| p.as_i64()) {
                                    token_ids.push(id);
                                }
                            }
                        }
                    }
                    PyRep::Integer(v) => {
                        if ctx.db.with_snapshot(|s| s.characters.contains_key(v)) {
                            target_cid = Some(*v);
                        } else {
                            token_ids.push(*v);
                        }
                    }
                    _ => {}
                }
            }
            let Some(cid) = target_cid else {
                return Err(err(
                    "CharacterNotSelected",
                    "ClaimRedeemTokens requires a target characterID",
                ));
            };
            let (claimed, created, loc) = try_with_store(ctx, |snap, store| {
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
                let dest_loc = ch
                    .station_id
                    .or(ch.structure_id)
                    .unwrap_or(ch.home_station_id);
                let list = store.redeem_tokens.entry(uid).or_default();
                let mut claimed_toks = Vec::new();
                let mut created_items = Vec::new();
                let mut remaining = Vec::new();
                for tok in list.drain(..) {
                    if token_ids.is_empty() || token_ids.contains(&tok.token_id) {
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
                Ok((claimed_toks, created_items, dest_loc))
            })?;
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
            let slots = with_store(ctx, |_, store| {
                store.mct_slots.get(&uid).cloned().unwrap_or_default()
            });
            let dict: Vec<(PyRep, PyRep)> = slots
                .into_iter()
                .enumerate()
                .map(|(idx, exp)| {
                    (
                        PyRep::Integer(idx as i64 + 1),
                        PyRep::keyval([
                            ("slotID", PyRep::Integer(idx as i64 + 1)),
                            ("expiryDate", PyRep::Integer(exp as i64)),
                            ("active", PyRep::Bool(true)),
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
                    (it.type_id == 34189 || it.type_id == 48582)
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
            with_store(ctx, |_, store| {
                store.mct_slots.entry(uid).or_default().push(expiry);
            });
            Ok(Some(PyRep::keyval([
                ("expiryDate", PyRep::Integer(expiry as i64)),
                ("slotActivated", PyRep::Bool(true)),
            ])))
        }
        ("userSvc", "GetUpdatedMultiCharacterTraining") | ("userSvc", "InvalidateMultiCharacterTrainingCache") => {
            let uid = ServerContext::require_auth(session)?;
            let slots = with_store(ctx, |_, store| {
                store.mct_slots.get(&uid).cloned().unwrap_or_default()
            });
            let list = slots.into_iter().map(|exp| PyRep::keyval([
                ("expiryDate", PyRep::Integer(exp as i64)),
                ("active", PyRep::Bool(true)),
            ])).collect();
            Ok(Some(PyRep::List(list)))
        }
        ("userSvc", "ActivateCharacterReSculpt") => {
            let (_, char_id) = ServerContext::require_character(session)?;
            let res: Result<(), BridgeRpcError> = ctx.db.try_with_mut_snapshot(|snap| {
                let cert_found = snap.items.values_mut().find(|it| {
                    it.type_id == 28662 && it.owner_id == char_id && it.quantity >= 1
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
            with_store(ctx, |_, store| {
                store.resculpt_pending.insert(char_id);
            });
            Ok(Some(PyRep::Bool(true)))
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
        ("petitioner", "CreatePetition") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let subject = args.first().and_then(|a| a.as_str()).map(str::trim).filter(|s| !s.is_empty()).unwrap_or("");
            if subject.is_empty() {
                return Err(err("InvalidPetitionSubject", "CreatePetition requires a non-empty subject"));
            }
            let desc = args.get(1).and_then(|a| a.as_str()).unwrap_or("Support request").to_string();
            let cat_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(1);
            if !CANONICAL_PETITION_CATEGORIES.iter().any(|&(id, _, _, _, _)| id == cat_id) {
                return Err(err("InvalidPetitionCategory", format!("Unknown petition category {cat_id}")));
            }
            let now_unix = ctx.now_unix();
            let pet_id = with_store(ctx, |_, store| {
                let id = store.alloc_id();
                let mid = store.alloc_message_id();
                store.petitions.insert(
                    id,
                    PetitionEntry {
                        petition_id: id,
                        petitioner_cid: cid,
                        subject: subject.to_string(),
                        description: desc.clone(),
                        category_id: cat_id,
                        queue_id: 1,
                        status: "open".into(),
                        claimed_by: None,
                        escalated: false,
                        unread: false,
                        last_unread_message_id: mid,
                        rating: None, response_time_rating: None,
                        helpfulness_rating: None, attitude_rating: None,
                        rating_comment: String::new(),
                        messages: vec![(cid, desc, now_unix)],
                        logs: vec![("Created".into(), cid, now_unix)],
                    },
                );
                id
            });
            ctx.notify(NotificationTarget::Character(cid), "OnPetitionCreated", "charid", vec![PyRep::Integer(pet_id)]);
            Ok(Some(PyRep::keyval([
                ("petitionID", PyRep::Integer(pet_id)), ("status", PyRep::String("open".into())), ("categoryID", PyRep::Integer(cat_id)),
            ])))
        }
        ("petitioner", "GetMyPetitionsEx") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let list = ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                let mut rows: Vec<_> = store.petitions.values().filter(|p| p.petitioner_cid == cid).cloned().collect();
                rows.sort_by_key(|p| p.petition_id);
                rows.into_iter().map(|p| PyRep::keyval([
                    ("petitionID", PyRep::Integer(p.petition_id)), ("subject", PyRep::String(p.subject)),
                    ("description", PyRep::String(p.description)), ("categoryID", PyRep::Integer(p.category_id)),
                    ("status", PyRep::String(p.status)), ("escalated", PyRep::Bool(p.escalated)),
                    ("claimedBy", p.claimed_by.map(PyRep::Integer).unwrap_or(PyRep::None)),
                    ("rating", p.rating.map(PyRep::Integer).unwrap_or(PyRep::None)), ("responseTimeRating", p.response_time_rating.map(PyRep::Integer).unwrap_or(PyRep::None)),
                    ("helpfulnessRating", p.helpfulness_rating.map(PyRep::Integer).unwrap_or(PyRep::None)), ("attitudeRating", p.attitude_rating.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ])).collect()
            });
            Ok(Some(PyRep::List(list)))
        }
        ("petitioner", "HasOpenTickets") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let has_open = ctx.db.with_snapshot(|snap| {
                load_store(snap).petitions.values().any(|p| p.petitioner_cid == cid && p.status != "closed" && p.status != "cancelled")
            });
            Ok(Some(PyRep::Bool(has_open)))
        }
        ("petitioner", "CancelPetition") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let pet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let now_unix = ctx.now_unix();
            try_with_store(ctx, |_, store| {
                let Some(pet) = store.petitions.get_mut(&pet_id) else {
                    return Err(err("PetitionNotFound", format!("Petition {pet_id} not found")));
                };
                if pet.petitioner_cid != cid && !session.has_gm_role() {
                    return Err(err("InsufficientRole", "Cannot cancel another player's petition"));
                }
                if pet.status.eq_ignore_ascii_case("closed") {
                    return Err(err("PetitionClosed", "Cannot cancel a closed petition"));
                }
                pet.status = "cancelled".into();
                pet.logs.push(("Cancelled".into(), cid, now_unix));
                Ok(())
            })?;
            Ok(Some(PyRep::Bool(true)))
        }
        ("petitioner", "DeletePetition") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let pet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            try_with_store(ctx, |_, store| {
                let Some(pet) = store.petitions.get(&pet_id) else {
                    return Err(err("PetitionNotFound", format!("Petition {pet_id} not found")));
                };
                if pet.petitioner_cid != cid && !session.has_gm_role() {
                    return Err(err("InsufficientRole", "Cannot delete another player's petition"));
                }
                if !pet.status.eq_ignore_ascii_case("closed") && !pet.status.eq_ignore_ascii_case("cancelled") {
                    return Err(err("PetitionNotClosed", "Cannot delete an active petition"));
                }
                store.petitions.remove(&pet_id);
                Ok(())
            })?;
            Ok(Some(PyRep::Bool(true)))
        }
        ("petitioner", "PetitionerChat") | ("petitioner", "PetitioneeChat") => {
            let (_, cid) = ServerContext::require_character(session)?;
            if method == "PetitioneeChat" { req_gm(session, "PetitioneeChat")?; }
            let pet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let msg = args.get(1).and_then(|a| a.as_str()).unwrap_or("").to_string();
            let now_unix = ctx.now_unix();
            let cnt = try_with_store(ctx, |_, store| {
                let is_gm_msg = {
                    let Some(pet) = store.petitions.get(&pet_id) else {
                        return Err(err("PetitionNotFound", format!("Petition {pet_id} not found")));
                    };
                    if pet.status.eq_ignore_ascii_case("closed") || pet.status.eq_ignore_ascii_case("cancelled") {
                        return Err(err("PetitionClosed", "Cannot post chat messages to a closed or cancelled petition"));
                    }
                    if method == "PetitionerChat" && pet.petitioner_cid != cid {
                        return Err(err("AccessDenied", "Only the petitioner may post player messages on this petition"));
                    }
                    method == "PetitioneeChat" || cid != pet.petitioner_cid
                };
                let mid = if is_gm_msg { store.alloc_message_id() } else { 0 };
                let pet = store.petitions.get_mut(&pet_id).expect("verified above");
                pet.messages.push((cid, msg.clone(), now_unix));
                if is_gm_msg {
                    pet.unread = true;
                    pet.last_unread_message_id = mid;
                    pet.status = "Responded".into();
                    pet.logs.push((format!("GMComment:{msg}"), cid, now_unix));
                } else {
                    pet.logs.push((format!("PlayerComment:{msg}"), cid, now_unix));
                }
                Ok(pet.messages.len())
            })?;
            Ok(Some(PyRep::keyval([("petitionID", PyRep::Integer(pet_id)), ("messageCount", PyRep::Integer(cnt as i64))])))
        }
        ("petitioner", "GetPetitionQueue") | ("petitioner", "GetClaimedPetitions") => {
            let (_, cid) = ServerContext::require_character(session)?;
            req_gm(session, method)?;
            let target_qid = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let list = ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                let mut rows: Vec<_> = store.petitions.values().filter(|p| {
                    if method == "GetClaimedPetitions" { p.claimed_by == Some(cid) }
                    else { p.status != "closed" && p.status != "cancelled" && p.queue_id == target_qid }
                }).collect();
                rows.sort_by_key(|p| p.petition_id);
                rows.into_iter().map(|p| PyRep::keyval([
                    ("petitionID", PyRep::Integer(p.petition_id)), ("petitionerID", PyRep::Integer(p.petitioner_cid)),
                    ("subject", PyRep::String(p.subject.clone())), ("status", PyRep::String(p.status.clone())),
                    ("escalated", PyRep::Bool(p.escalated)), ("queueID", PyRep::Integer(p.queue_id)),
                    ("claimedBy", p.claimed_by.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ])).collect()
            });
            Ok(Some(PyRep::List(list)))
        }
        ("petitioner", "ClaimPetition") => {
            let (_, cid) = ServerContext::require_character(session)?;
            req_gm(session, "ClaimPetition")?;
            let pet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let now_unix = ctx.now_unix();
            try_with_store(ctx, |_, store| {
                let Some(pet) = store.petitions.get_mut(&pet_id) else {
                    return Err(err("PetitionNotFound", format!("Petition {pet_id} not found")));
                };
                if pet.status.eq_ignore_ascii_case("closed") || pet.status.eq_ignore_ascii_case("cancelled") {
                    return Err(err("PetitionClosed", "Cannot claim a closed or cancelled petition"));
                }
                if pet.claimed_by.is_some_and(|c| c != cid) {
                    return Err(err("PetitionAlreadyClaimed", "Petition already claimed by another GM"));
                }
                pet.claimed_by = Some(cid);
                pet.status = "claimed".into();
                pet.logs.push(("Claimed".into(), cid, now_unix));
                Ok(())
            })?;
            Ok(Some(PyRep::Integer(1)))
        }
        ("petitioner", "UnClaimPetition")
        | ("petitioner", "EscalatePetition")
        | ("petitioner", "ClosePetition") => {
            let (_, cid) = ServerContext::require_character(session)?;
            req_gm(session, method)?;
            let now_unix = ctx.now_unix();
            let pet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let target_qid = args.get(1).and_then(|a| a.as_i64()).unwrap_or(2);
            let note = args.get(1).and_then(|a| a.as_str()).or_else(|| args.get(2).and_then(|a| a.as_str())).unwrap_or("");
            let st = try_with_store(ctx, |_, store| {
                if !store.petitions.contains_key(&pet_id) {
                    return Err(err("PetitionNotFound", format!("Petition {pet_id} not found")));
                }
                let mid = if method == "ClosePetition" && !note.is_empty() { Some(store.alloc_message_id()) } else { None };
                let pet = store.petitions.get_mut(&pet_id).expect("verified above");
                match method {
                    "UnClaimPetition" => {
                        pet.claimed_by = None;
                        pet.status = "open".into();
                        pet.logs.push(("Unclaimed".into(), cid, now_unix));
                    }
                    "EscalatePetition" => {
                        if !CANONICAL_PETITION_QUEUES.iter().any(|&(qid, _)| qid == target_qid) {
                            return Err(err("InvalidPetitionQueue", format!("Unknown petition queue {target_qid}")));
                        }
                        pet.queue_id = target_qid;
                        pet.claimed_by = None;
                        pet.escalated = true;
                        pet.status = "escalated".into();
                        pet.logs.push((if note.is_empty() { "Escalated".into() } else { format!("Escalated:{note}") }, cid, now_unix));
                    }
                    "ClosePetition" => {
                        pet.status = "closed".into();
                        if let Some(m) = mid {
                            pet.messages.push((cid, note.to_string(), now_unix));
                            pet.unread = true;
                            pet.last_unread_message_id = m;
                        }
                        pet.logs.push((format!("Closed:{note}"), cid, now_unix));
                    }
                    _ => {}
                }
                Ok(pet.status.clone())
            })?;
            Ok(Some(PyRep::keyval([("petitionID", PyRep::Integer(pet_id)), ("status", PyRep::String(st)), ("gmID", PyRep::Integer(cid))])))
        }
        ("petitioner", "GetPetitionMessages") | ("petitioner", "GetLog") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let pet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let Some(pet) = ctx.db.with_snapshot(|snap| load_store(snap).petitions.get(&pet_id).cloned()) else {
                return Err(err("PetitionNotFound", format!("Petition {pet_id} not found")));
            };
            if pet.petitioner_cid != cid && !session.has_gm_role() {
                return Err(err("AccessDenied", "Only the petitioner or a GM may read petition messages and logs"));
            }
            if method == "GetPetitionMessages" {
                let msgs = pet.messages.into_iter().map(|(sender, text, ts)| PyRep::keyval([("senderID", PyRep::Integer(sender)), ("text", PyRep::String(text)), ("timestamp", PyRep::Integer(ts as i64))])).collect();
                Ok(Some(PyRep::List(msgs)))
            } else {
                let logs = pet.logs.into_iter().map(|(ev, actor, ts)| {
                    let prefix = ev.split(':').next().unwrap_or("");
                    let eid = match prefix {
                        "Created" => 1,
                        "Claimed" => 2,
                        "Unclaimed" => 3,
                        "Escalated" => 4,
                        "Closed" => 5,
                        "GMComment" | "PlayerComment" => 6,
                        "Rated" => 7,
                        "Cancelled" => 8,
                        _ => 1,
                    };
                    PyRep::keyval([
                        ("event", PyRep::String(ev.clone())), ("eventText", PyRep::String(ev)),
                        ("actorID", PyRep::Integer(actor)), ("timestamp", PyRep::Integer(ts as i64)),
                        ("characterID", PyRep::Integer(actor)), ("eventID", PyRep::Integer(eid)),
                        ("logDateTime", PyRep::Integer(ts as i64 * 10_000_000 + 116_444_736_000_000_000)),
                        ("userName", PyRep::String(format!("char_{actor}"))),
                    ])
                }).collect();
                Ok(Some(PyRep::List(logs)))
            }
        }
        ("petitioner", "AddPetitionRating") | ("petitioner", "UpdatePetitionRating") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let pet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let resp_rating = args.get(1).and_then(|a| a.as_i64()).unwrap_or(10).clamp(0, 10);
            let help_rating = args.get(2).and_then(|a| a.as_i64()).unwrap_or(resp_rating).clamp(0, 10);
            let att_rating = args.get(3).and_then(|a| a.as_i64()).unwrap_or(resp_rating).clamp(0, 10);
            let comment = args.get(4).and_then(|a| a.as_str()).or_else(|| args.get(2).and_then(|a| a.as_str())).unwrap_or("").to_string();
            let now_unix = ctx.now_unix();
            try_with_store(ctx, |_, store| {
                let Some(pet) = store.petitions.get_mut(&pet_id) else {
                    return Err(err("PetitionNotFound", format!("Petition {pet_id} not found")));
                };
                if pet.petitioner_cid != cid {
                    return Err(err("CharacterOwnershipMismatch", "Only the petitioner can rate a petition"));
                }
                if pet.status != "closed" {
                    return Err(err("PetitionNotClosed", "Petition must be closed before rating"));
                }
                pet.rating = Some(resp_rating);
                pet.response_time_rating = Some(resp_rating);
                pet.helpfulness_rating = Some(help_rating);
                pet.attitude_rating = Some(att_rating);
                pet.rating_comment = comment.clone();
                pet.logs.push((format!("Rated:{resp_rating}"), cid, now_unix));
                Ok(())
            })?;
            Ok(Some(PyRep::keyval([
                ("petitionID", PyRep::Integer(pet_id)), ("rating", PyRep::Integer(resp_rating)),
                ("responseTimeRating", PyRep::Integer(resp_rating)), ("helpfulnessRating", PyRep::Integer(help_rating)),
                ("attitudeRating", PyRep::Integer(att_rating)), ("comment", PyRep::String(comment)),
            ])))
        }
        ("petitioner", "GetCategories") => {
            let _ = ServerContext::require_character(session)?;
            let cats = CANONICAL_PETITION_CATEGORIES.iter().map(|&(id, _, name, _, _)| {
                PyRep::keyval([("categoryID", PyRep::Integer(id)), ("categoryName", PyRep::String(name.into()))])
            }).collect();
            Ok(Some(PyRep::List(cats)))
        }
        ("petitioner", "GetCategoryHierarchicalInfo") => {
            let _ = ServerContext::require_character(session)?;
            let mut parent_dict = Vec::new();
            let mut child_map: HashMap<i64, Vec<(PyRep, PyRep)>> = HashMap::new();
            let mut desc_dict = Vec::new();
            let mut billing_list = Vec::new();
            for &(id, pid, name, desc, is_billing) in CANONICAL_PETITION_CATEGORIES {
                let entry = PyRep::Tuple(vec![PyRep::String(name.into()), PyRep::String("EN".into())]);
                if pid == 0 {
                    parent_dict.push((PyRep::Integer(id), entry));
                    child_map.entry(id).or_default();
                } else {
                    child_map.entry(pid).or_default().push((PyRep::Integer(id), entry));
                }
                desc_dict.push((PyRep::Integer(id), PyRep::String(desc.into())));
                if is_billing { billing_list.push(PyRep::Integer(id)); }
            }
            let mut child_dict: Vec<(PyRep, PyRep)> = child_map.into_iter().map(|(pid, children)| (PyRep::Integer(pid), PyRep::Dict(children))).collect();
            child_dict.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));
            Ok(Some(PyRep::Tuple(vec![PyRep::Dict(parent_dict), PyRep::Dict(child_dict), PyRep::Dict(desc_dict), PyRep::List(billing_list)])))
        }
        ("petitioner", "GetCategoryProperties") => {
            let _ = ServerContext::require_character(session)?;
            Ok(Some(PyRep::List(Vec::new())))
        }
        ("petitioner", "GetClientPickerInfo") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let lookup = args.first().and_then(|a| a.as_str()).unwrap_or("").to_ascii_lowercase();
            let items = ctx.db.with_snapshot(|snap| {
                let mut rows: Vec<(i64, String)> = snap.items.values()
                    .filter(|it| it.owner_id == cid)
                    .map(|it| {
                        let tname = ctx.sde.types.get(&it.type_id).map(|t| t.name.clone()).unwrap_or_else(|| format!("Item {}", it.type_id));
                        (it.item_id, tname)
                    })
                    .filter(|(_, name)| lookup.is_empty() || name.to_ascii_lowercase().contains(&lookup))
                    .collect();
                rows.sort_by_key(|(id, _)| *id);
                if rows.is_empty() {
                    if let Some(ch) = snap.characters.get(&cid) {
                        let sname = ctx.sde.solar_systems.get(&ch.solar_system_id).map(|s| s.name.clone()).unwrap_or_else(|| format!("System {}", ch.solar_system_id));
                        rows.push((i64::from(ch.solar_system_id), sname));
                    }
                }
                rows.into_iter().map(|(id, label)| PyRep::Tuple(vec![PyRep::String(label), PyRep::Integer(id)])).collect()
            });
            Ok(Some(PyRep::List(items)))
        }
        ("petitioner", "GetQueues") => {
            let _ = ServerContext::require_character(session)?;
            req_gm(session, "GetQueues")?;
            let queues = CANONICAL_PETITION_QUEUES.iter().map(|&(id, name)| {
                PyRep::keyval([("queueID", PyRep::Integer(id)), ("queueName", PyRep::String(name.into()))])
            }).collect();
            Ok(Some(PyRep::List(queues)))
        }
        ("petitioner", "GetUnreadMessages") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let unread = ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                let mut rows: Vec<_> = store.petitions.values().filter(|p| p.petitioner_cid == cid && p.unread).collect();
                rows.sort_by_key(|p| p.petition_id);
                rows.into_iter().map(|p| {
                    let last_text = p.messages.iter().rev().find(|(s, _, _)| *s != p.petitioner_cid).or_else(|| p.messages.last()).map(|(_, t, _)| t.clone()).unwrap_or_else(|| p.description.clone());
                    let mid = if p.last_unread_message_id > 0 { p.last_unread_message_id } else { p.messages.len() as i64 };
                    PyRep::keyval([("petitionID", PyRep::Integer(p.petition_id)), ("text", PyRep::String(last_text)), ("messageID", PyRep::Integer(mid))])
                }).collect()
            });
            Ok(Some(PyRep::List(unread)))
        }
        ("petitioner", "GetUserCatalogCountry") => {
            let (uid, _) = ServerContext::require_character(session)?;
            // DERIVED: default catalog country code "US" when account country is unset in store.
            let country = ctx.db.with_snapshot(|snap| {
                load_store(snap).user_countries.get(&uid).cloned().unwrap_or_else(|| "US".into())
            });
            Ok(Some(PyRep::String(country)))
        }
        ("petitioner", "GetZendeskJwtLink") => {
            let (uid, cid) = ServerContext::require_character(session)?;
            let return_to = args.first().and_then(|a| a.as_str()).unwrap_or("hc/en-us");
            // DERIVED: deterministic Zendesk SSO redirect URL with character/account identity when Zendesk support is enabled.
            let link = ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                if store.zendesk_disabled {
                    PyRep::None
                } else {
                    let cname = snap.characters.get(&cid).map(|c| c.name.replace(' ', "_")).unwrap_or_else(|| format!("char_{cid}"));
                    PyRep::String(format!("https://support.eveonline.com/access/jwt?jwt=eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.uid_{uid}_cid_{cid}_{cname}&return_to={return_to}"))
                }
            });
            Ok(Some(link))
        }
        ("petitioner", "IsSerenity") => {
            // const-ok: returns false on Tranquility and Western clusters
            let _ = ServerContext::require_character(session)?;
            Ok(Some(PyRep::Bool(false)))
        }
        ("petitioner", "IsZendeskEnabled") => {
            let _ = ServerContext::require_character(session)?;
            let enabled = ctx.db.with_snapshot(|snap| !load_store(snap).zendesk_disabled);
            Ok(Some(PyRep::Bool(enabled)))
        }
        ("petitioner", "IsZendeskSwapEnabled") => {
            // const-ok: Zendesk swap migration flag is disabled
            let _ = ServerContext::require_character(session)?;
            Ok(Some(PyRep::Bool(false)))
        }
        ("petitioner", "MarkAsRead") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let pid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            try_with_store(ctx, |_, store| {
                let Some(p) = store.petitions.get_mut(&pid) else {
                    return Err(err("PetitionNotFound", format!("Petition {pid} not found")));
                };
                if p.petitioner_cid != cid && !session.has_gm_role() {
                    return Err(err("AccessDenied", "Cannot mark another player's petition as read"));
                }
                p.unread = false;
                if p.status == "Responded" { p.status = "open".into(); }
                Ok(())
            })?;
            Ok(Some(PyRep::Bool(true)))
        }
        ("petitioner", "MayPetition") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let can = ctx.db.get_character(cid).is_ok();
            Ok(Some(PyRep::Bool(can)))
        }
        ("petitioner", "PropertyPopulationInfo") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let prop_str = args.first().and_then(|a| a.as_str()).or_else(|| args.get(1).and_then(|a| a.as_str()));
            let prop_id = if prop_str.is_none() { args.get(1).and_then(|a| a.as_i64()).or_else(|| args.first().and_then(|a| a.as_i64())) } else { None };
            let options = ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&cid) else { return Vec::new(); };
                if prop_str.is_some_and(|s| s.eq_ignore_ascii_case("SolarSystem") || s.eq_ignore_ascii_case("location")) || prop_id == Some(2) {
                    let sname = ctx.sde.solar_systems.get(&ch.solar_system_id).map(|s| s.name.clone()).unwrap_or_else(|| format!("System {}", ch.solar_system_id));
                    return vec![PyRep::Tuple(vec![PyRep::String(sname), PyRep::Integer(i64::from(ch.solar_system_id))])];
                }
                if prop_str.is_some_and(|s| s.eq_ignore_ascii_case("Character")) || prop_id == Some(4) {
                    let mut chars: Vec<_> = snap.characters.values().filter(|c| c.user_id == ch.user_id).map(|c| (c.character_id, c.name.clone())).collect();
                    chars.sort_by_key(|(id, _)| *id);
                    return chars.into_iter().map(|(id, n)| PyRep::Tuple(vec![PyRep::String(n), PyRep::Integer(id)])).collect();
                }
                let mut ships: Vec<(i64, String)> = snap.items.values()
                    .filter(|it| it.owner_id == cid && (it.item_id == ch.active_ship_id || ctx.sde.types.get(&it.type_id).is_some_and(|t| t.category_id == 6)))
                    .map(|it| {
                        let tname = ctx.sde.types.get(&it.type_id).map(|t| t.name.clone()).unwrap_or_else(|| format!("Ship {}", it.type_id));
                        (it.item_id, tname)
                    })
                    .collect();
                ships.sort_by_key(|(id, _)| *id);
                ships.into_iter().map(|(id, label)| PyRep::Tuple(vec![PyRep::String(label), PyRep::Integer(id)])).collect()
            });
            Ok(Some(PyRep::List(options)))
        }
        _ => Ok(None),
    }
}
