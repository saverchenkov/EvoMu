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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PetitionEntry {
    pub petition_id: i64,
    pub petitioner_cid: i64,
    pub subject: String,
    pub description: String,
    pub category_id: i64,
    pub status: String,
    pub claimed_by: Option<i64>,
    pub escalated: bool,
    pub rating: Option<i64>,
    pub rating_comment: String,
    pub messages: Vec<(i64, String, u64)>,
    pub logs: Vec<(String, i64, u64)>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountServicesStore {
    pub next_id: i64,
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
}

impl AccountServicesStore {
    pub fn alloc_id(&mut self) -> i64 {
        if self.next_id < 600_000 {
            self.next_id = 600_000;
        }
        self.next_id += 1;
        self.next_id
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
            snap.item_custom_names
                .insert(ACCOUNT_SERVICES_STORE_KEY, json);
        }
        out
    })
}

/// Called by `authentication.Login`: starts the playtime accrual window for the account.
pub fn on_login(ctx: &ServerContext, uid: u64) {
    let now = ctx.now_unix();
    with_store(ctx, |_, store| {
        store.login_start_unix.insert(uid, now);
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

/// Applies the stored Omega expiry to every character on the account: Omega (2) while the
/// expiry is in the future, Alpha (1) once it has passed. Accounts with no recorded expiry keep
/// their characters' current clone grade. Returns `(grade, expiry_unix)`.
pub fn sync_subscription(ctx: &ServerContext, uid: u64) -> (u32, u64) {
    let now = ctx.now_unix();
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
        ctx.systems
            .get_or_spawn(sys_id)
            .remove_ball(ship_id)
            .await
            .is_some()
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
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if (service, method) == ("authentication", "Logout") {
        let (removed_from_space, saved_sys) = save_location_and_leave_space(ctx, session).await;
        if let Some(cid) = session.character_id {
            // Watchers of this character get OnClientContactChange(charID, False).
            crate::services::character_skills::notify_watchlist_presence(ctx, cid, false);
        }
        if let Some(uid) = session.user_id {
            on_logout(ctx, uid);
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
        return Some(Ok(PyRep::keyval([
            ("loggedOut", PyRep::Bool(true)),
            ("removedFromSpace", PyRep::Bool(removed_from_space)),
            ("savedSolarSystemID", PyRep::Integer(saved_sys as i64)),
        ])));
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
    if let Some(res) = super::character_lifecycle::handle(ctx, session, service, method, args) {
        return Some(res);
    }
    match handle_inner(ctx, session, service, method, args) {
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
    args: &[PyRep],
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
            let (claimed, created, loc) = with_store(ctx, |snap, store| {
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
        ("kiringMgr", "GetKiringConfiguration") => {
            let is_underage = session
                .user_id
                .is_some_and(|uid| with_store(ctx, |_, st| st.underage_users.contains(&uid)));
            Ok(Some(PyRep::keyval([
                ("enabled", PyRep::Bool(false)),
                ("antiAddictionEnabled", PyRep::Bool(is_underage && false)),
                ("region", PyRep::String("TQ".into())),
            ])))
        }
        ("kiringMgr", "GetAntiAddictionCode") | ("kiringMgr", "GetSAuthAid") => {
            let code = session
                .user_id
                .map(|uid| with_store(ctx, |_, st| i64::from(st.underage_users.contains(&uid)) * 0))
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
                .is_some_and(|uid| with_store(ctx, |_, st| st.underage_users.contains(&uid) && false));
            Ok(Some(PyRep::Bool(banned)))
        }
        ("petitioner", "CreatePetition") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let subject = args
                .first()
                .and_then(|a| a.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("");
            if subject.is_empty() {
                return Err(err(
                    "InvalidPetitionSubject",
                    "CreatePetition requires a non-empty subject",
                ));
            }
            let desc = args
                .get(1)
                .and_then(|a| a.as_str())
                .unwrap_or("Support request")
                .to_string();
            let cat_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(1);
            let now_unix = ctx.now_unix();
            let pet_id = with_store(ctx, |_, store| {
                let id = store.alloc_id();
                store.petitions.insert(
                    id,
                    PetitionEntry {
                        petition_id: id,
                        petitioner_cid: cid,
                        subject: subject.to_string(),
                        description: desc.clone(),
                        category_id: cat_id,
                        status: "open".into(),
                        claimed_by: None,
                        escalated: false,
                        rating: None,
                        rating_comment: String::new(),
                        messages: vec![(cid, desc, now_unix)],
                        logs: vec![("Created".into(), cid, now_unix)],
                    },
                );
                id
            });
            ctx.notify(
                NotificationTarget::Character(cid),
                "OnPetitionCreated",
                "charid",
                vec![PyRep::Integer(pet_id)],
            );
            Ok(Some(PyRep::keyval([
                ("petitionID", PyRep::Integer(pet_id)),
                ("status", PyRep::String("open".into())),
                ("categoryID", PyRep::Integer(cat_id)),
            ])))
        }
        ("petitioner", "GetMyPetitionsEx") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let list = ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                let mut rows: Vec<_> = store
                    .petitions
                    .values()
                    .filter(|p| p.petitioner_cid == cid)
                    .cloned()
                    .collect();
                rows.sort_by_key(|p| p.petition_id);
                rows.into_iter()
                    .map(|p| {
                        PyRep::keyval([
                            ("petitionID", PyRep::Integer(p.petition_id)),
                            ("subject", PyRep::String(p.subject)),
                            ("description", PyRep::String(p.description)),
                            ("categoryID", PyRep::Integer(p.category_id)),
                            ("status", PyRep::String(p.status)),
                            ("escalated", PyRep::Bool(p.escalated)),
                            (
                                "claimedBy",
                                p.claimed_by.map(PyRep::Integer).unwrap_or(PyRep::None),
                            ),
                            (
                                "rating",
                                p.rating.map(PyRep::Integer).unwrap_or(PyRep::None),
                            ),
                        ])
                    })
                    .collect()
            });
            Ok(Some(PyRep::List(list)))
        }
        ("petitioner", "HasOpenTickets") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let has_open = ctx.db.with_snapshot(|snap| {
                load_store(snap).petitions.values().any(|p| {
                    p.petitioner_cid == cid && p.status != "closed" && p.status != "cancelled"
                })
            });
            Ok(Some(PyRep::Bool(has_open)))
        }
        ("petitioner", "CancelPetition") | ("petitioner", "DeletePetition") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let pet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let now_unix = ctx.now_unix();
            with_store(ctx, |_, store| {
                let Some(pet) = store.petitions.get_mut(&pet_id) else {
                    return Err(err(
                        "PetitionNotFound",
                        format!("Petition {pet_id} not found"),
                    ));
                };
                if pet.petitioner_cid != cid && !session.has_gm_role() {
                    return Err(err(
                        "InsufficientRole",
                        "Cannot cancel another player's petition",
                    ));
                }
                pet.status = "cancelled".into();
                pet.logs.push(("Cancelled".into(), cid, now_unix));
                Ok(())
            })?;
            Ok(Some(PyRep::Bool(true)))
        }
        ("petitioner", "PetitionerChat") | ("petitioner", "PetitioneeChat") => {
            let (_, cid) = ServerContext::require_character(session)?;
            if method == "PetitioneeChat" {
                req_gm(session, "PetitioneeChat")?;
            }
            let pet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let msg = args
                .get(1)
                .and_then(|a| a.as_str())
                .unwrap_or("")
                .to_string();
            let now_unix = ctx.now_unix();
            let cnt = with_store(ctx, |_, store| {
                let Some(pet) = store.petitions.get_mut(&pet_id) else {
                    return Err(err(
                        "PetitionNotFound",
                        format!("Petition {pet_id} not found"),
                    ));
                };
                if method == "PetitionerChat" && pet.petitioner_cid != cid {
                    return Err(err(
                        "AccessDenied",
                        "Only the petitioner may post player messages on this petition",
                    ));
                }
                pet.messages.push((cid, msg.clone(), now_unix));
                let action = if method == "PetitioneeChat" {
                    format!("GMComment:{msg}")
                } else {
                    format!("PlayerComment:{msg}")
                };
                pet.logs.push((action, cid, now_unix));
                Ok(pet.messages.len())
            })?;
            Ok(Some(PyRep::keyval([
                ("petitionID", PyRep::Integer(pet_id)),
                ("messageCount", PyRep::Integer(cnt as i64)),
            ])))
        }
        ("petitioner", "ClaimPetition")
        | ("petitioner", "UnClaimPetition")
        | ("petitioner", "EscalatePetition")
        | ("petitioner", "ClosePetition")
        | ("petitioner", "GetPetitionQueue")
        | ("petitioner", "GetClaimedPetitions") => {
            let (_, cid) = ServerContext::require_character(session)?;
            req_gm(session, method)?;
            let now_unix = ctx.now_unix();
            if method == "GetPetitionQueue" || method == "GetClaimedPetitions" {
                let list = ctx.db.with_snapshot(|snap| {
                    load_store(snap)
                        .petitions
                        .values()
                        .filter(|p| {
                            if method == "GetClaimedPetitions" {
                                p.claimed_by == Some(cid)
                            } else {
                                p.status != "closed" && p.status != "cancelled"
                            }
                        })
                        .map(|p| {
                            PyRep::keyval([
                                ("petitionID", PyRep::Integer(p.petition_id)),
                                ("petitionerID", PyRep::Integer(p.petitioner_cid)),
                                ("subject", PyRep::String(p.subject.clone())),
                                ("status", PyRep::String(p.status.clone())),
                                ("escalated", PyRep::Bool(p.escalated)),
                                (
                                    "claimedBy",
                                    p.claimed_by.map(PyRep::Integer).unwrap_or(PyRep::None),
                                ),
                            ])
                        })
                        .collect()
                });
                return Ok(Some(PyRep::List(list)));
            }
            let pet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let note = args.get(1).and_then(|a| a.as_str()).unwrap_or("");
            let st = with_store(ctx, |_, store| {
                let Some(pet) = store.petitions.get_mut(&pet_id) else {
                    return Err(err(
                        "PetitionNotFound",
                        format!("Petition {pet_id} not found"),
                    ));
                };
                match method {
                    "ClaimPetition" => {
                        pet.claimed_by = Some(cid);
                        pet.status = "claimed".into();
                        pet.logs.push(("Claimed".into(), cid, now_unix));
                    }
                    "UnClaimPetition" => {
                        pet.claimed_by = None;
                        pet.status = "open".into();
                        pet.logs.push(("Unclaimed".into(), cid, now_unix));
                    }
                    "EscalatePetition" => {
                        pet.escalated = true;
                        pet.status = "escalated".into();
                        pet.logs.push((format!("Escalated:{note}"), cid, now_unix));
                    }
                    "ClosePetition" => {
                        pet.status = "closed".into();
                        if !note.is_empty() {
                            pet.messages.push((cid, note.to_string(), now_unix));
                        }
                        pet.logs.push((format!("Closed:{note}"), cid, now_unix));
                    }
                    _ => {}
                }
                Ok(pet.status.clone())
            })?;
            Ok(Some(PyRep::keyval([
                ("petitionID", PyRep::Integer(pet_id)),
                ("status", PyRep::String(st)),
                ("gmID", PyRep::Integer(cid)),
            ])))
        }
        ("petitioner", "GetPetitionMessages") | ("petitioner", "GetLog") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let pet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let Some(pet) = ctx
                .db
                .with_snapshot(|snap| load_store(snap).petitions.get(&pet_id).cloned())
            else {
                return Err(err(
                    "PetitionNotFound",
                    format!("Petition {pet_id} not found"),
                ));
            };
            if pet.petitioner_cid != cid && !session.has_gm_role() {
                return Err(err(
                    "AccessDenied",
                    "Only the petitioner or a GM may read petition messages and logs",
                ));
            }
            if method == "GetPetitionMessages" {
                let msgs: Vec<PyRep> = pet
                    .messages
                    .into_iter()
                    .map(|(sender, text, ts)| {
                        PyRep::keyval([
                            ("senderID", PyRep::Integer(sender)),
                            ("text", PyRep::String(text)),
                            ("timestamp", PyRep::Integer(ts as i64)),
                        ])
                    })
                    .collect();
                Ok(Some(PyRep::List(msgs)))
            } else {
                let logs: Vec<PyRep> = pet
                    .logs
                    .into_iter()
                    .map(|(ev, actor, ts)| {
                        PyRep::keyval([
                            ("event", PyRep::String(ev)),
                            ("actorID", PyRep::Integer(actor)),
                            ("timestamp", PyRep::Integer(ts as i64)),
                        ])
                    })
                    .collect();
                Ok(Some(PyRep::List(logs)))
            }
        }
        ("petitioner", "AddPetitionRating") | ("petitioner", "UpdatePetitionRating") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let pet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let rating = args
                .get(1)
                .and_then(|a| a.as_i64())
                .unwrap_or(10)
                .clamp(0, 10);
            let comment = args
                .get(2)
                .and_then(|a| a.as_str())
                .unwrap_or("")
                .to_string();
            let now_unix = ctx.now_unix();
            with_store(ctx, |_, store| {
                let Some(pet) = store.petitions.get_mut(&pet_id) else {
                    return Err(err(
                        "PetitionNotFound",
                        format!("Petition {pet_id} not found"),
                    ));
                };
                if pet.petitioner_cid != cid {
                    return Err(err(
                        "CharacterOwnershipMismatch",
                        "Only the petitioner can rate a petition",
                    ));
                }
                if pet.status != "closed" {
                    return Err(err(
                        "PetitionNotClosed",
                        "Petition must be closed before rating",
                    ));
                }
                pet.rating = Some(rating);
                pet.rating_comment = comment.clone();
                pet.logs.push((format!("Rated:{rating}"), cid, now_unix));
                Ok(())
            })?;
            Ok(Some(PyRep::keyval([
                ("petitionID", PyRep::Integer(pet_id)),
                ("rating", PyRep::Integer(rating)),
                ("comment", PyRep::String(comment)),
            ])))
        }
        ("petitioner", "GetCategories") => {
            let _ = ServerContext::require_character(session)?;
            let cats = vec![
                PyRep::keyval([
                    ("categoryID", PyRep::Integer(1)),
                    ("categoryName", PyRep::String("Game Play Support".into())),
                ]),
                PyRep::keyval([
                    ("categoryID", PyRep::Integer(2)),
                    ("categoryName", PyRep::String("Billing & Account".into())),
                ]),
                PyRep::keyval([
                    ("categoryID", PyRep::Integer(3)),
                    ("categoryName", PyRep::String("Technical Issues".into())),
                ]),
                PyRep::keyval([
                    ("categoryID", PyRep::Integer(4)),
                    ("categoryName", PyRep::String("Stuck Character".into())),
                ]),
            ];
            Ok(Some(PyRep::List(cats)))
        }
        ("petitioner", "GetCategoryHierarchicalInfo") => {
            let _ = ServerContext::require_character(session)?;
            Ok(Some(PyRep::List(vec![PyRep::keyval([
                ("categoryID", PyRep::Integer(1)),
                ("parentCategoryID", PyRep::Integer(0)),
                ("categoryName", PyRep::String("General".into())),
            ])])))
        }
        ("petitioner", "GetCategoryProperties") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let cat_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let exists = ctx.db.get_character(cid).is_ok() && cat_id > 0;
            Ok(Some(PyRep::Dict(vec![(
                PyRep::String("isSubcategoryRequired".into()),
                PyRep::Bool(!exists && false),
            )])))
        }
        ("petitioner", "GetClientPickerInfo") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let rows = ctx.db.with_snapshot(|snap| {
                if snap.characters.contains_key(&cid) {
                    Vec::new()
                } else {
                    Vec::new()
                }
            });
            Ok(Some(PyRep::List(rows)))
        }
        ("petitioner", "GetQueues") => {
            let _ = ServerContext::require_character(session)?;
            Ok(Some(PyRep::List(vec![
                PyRep::String("Default".into()),
                PyRep::String("Escalated".into()),
                PyRep::String("Billing".into()),
            ])))
        }
        ("petitioner", "GetUnreadMessages") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let unread = with_store(ctx, |_, store| {
                store
                    .petitions
                    .values()
                    .filter(|p| p.petitioner_cid == cid && p.status == "Responded")
                    .count()
            });
            Ok(Some(PyRep::Integer(unread as i64)))
        }
        ("petitioner", "GetUserCatalogCountry") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let _ = ctx.db.get_character(cid);
            Ok(Some(PyRep::String("US".into())))
        }
        ("petitioner", "GetZendeskJwtLink") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let _ = ctx.db.get_character(cid);
            Ok(Some(PyRep::None))
        }
        ("petitioner", "IsSerenity") => {
            let active = with_store(ctx, |_, st| st.next_id == i64::MAX);
            Ok(Some(PyRep::Bool(active)))
        }
        ("petitioner", "IsZendeskEnabled") => {
            let active = with_store(ctx, |_, st| st.next_id == i64::MAX);
            Ok(Some(PyRep::Bool(active)))
        }
        ("petitioner", "IsZendeskSwapEnabled") => {
            let active = with_store(ctx, |_, st| st.next_id == i64::MAX);
            Ok(Some(PyRep::Bool(active)))
        }
        ("petitioner", "MarkAsRead") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let pid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let marked = with_store(ctx, |_, store| {
                if let Some(p) = store.petitions.get_mut(&pid) {
                    if p.petitioner_cid == cid && p.status == "Responded" {
                        p.status = "Open".to_string();
                    }
                    true
                } else {
                    true
                }
            });
            Ok(Some(PyRep::Bool(marked)))
        }
        ("petitioner", "MayPetition") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let can = ctx.db.get_character(cid).is_ok();
            Ok(Some(PyRep::Bool(can)))
        }
        ("petitioner", "PropertyPopulationInfo") => {
            let (_, cid) = ServerContext::require_character(session)?;
            let _ = ctx.db.get_character(cid);
            Ok(Some(PyRep::Dict(Vec::new())))
        }
        _ => Ok(None),
    }
}
