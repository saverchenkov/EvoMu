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
pub const BIOMASS_DELAY_SECS: u64 = 10 * 3_600;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LifecycleStore {
    /// characterID -> unix time the delete was prepared.
    #[serde(default)]
    pub delete_prepared_unix: HashMap<i64, u64>,
    /// userID -> validation toggle state (`charUnboundMgr.ToggleValidation`).
    #[serde(default)]
    pub validation_toggled_by_user: HashMap<u64, bool>,
}

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
        client_error: None,
    }
}

pub fn load_store(snap: &eve_db::WorldStateSnapshot) -> LifecycleStore {
    snap.item_custom_names
        .get(&CHARACTER_LIFECYCLE_STORE_KEY)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

pub fn is_character_delete_pending(snap: &eve_db::WorldStateSnapshot, char_id: i64) -> bool {
    load_store(snap).delete_prepared_unix.contains_key(&char_id)
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
fn has_corp_director_or_personnel_manager_role(snap: &eve_db::WorldStateSnapshot, caller_char_id: i64, target_corp_id: i64) -> bool {
    if !snap.corporations.contains_key(&target_corp_id) {
        return false;
    }
    let (is_ceo, mut total_roles) =
        snap.effective_corp_roles_at(target_corp_id, caller_char_id, None, 0, 0);
    if is_ceo {
        return true;
    }
    if let Some(scoped) = snap.char_scoped_roles.get(&caller_char_id) {
        total_roles |= eve_db::corp_roles::encode_corp_role_names(&scoped.roles);
    }
    (total_roles & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_PERSONNEL_MANAGER)) != 0
}

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service == "charMgr" {
        return handle_char_mgr(ctx, session, method, args, kwargs);
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
            if let Some(name) = args.first().and_then(|a| a.as_str()) {
                if !crate::services::lookup_svc::is_word_allowed(name) {
                    return Some(Err(err("UserError", "CharacterNameContainsBannedWords")));
                }
            }
            None
        }
        ("charUnboundMgr", "SelectCharacterID") => {
            let char_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("charID").and_then(|v| v.as_i64()))?;
            let lock = ctx.db.with_snapshot(|s| {
                if let Some(blob) = s.cluster_and_runtime.character_locks.get(&char_id) {
                    return Some(format!("Character {char_id} is locked ({blob})"));
                }
                if load_store(s).delete_prepared_unix.contains_key(&char_id) {
                    return Some(format!("Character {char_id} is pending deletion"));
                }
                None
            });
            if let Some(reason) = lock {
                return Some(Err(err("CharacterLocked", reason)));
            }
            None
        }
        ("charUnboundMgr", "PrepareCharacterForDelete") => {
            Some(handle_prepare_character_delete(ctx, session, user_id, args))
        }
        ("charUnboundMgr", "CancelCharacterDeletePrepare") => {
            Some(handle_cancel_character_delete(ctx, user_id, args))
        }
        ("charUnboundMgr", "DeleteCharacter") => {
            Some(handle_delete_character(ctx, session, user_id, args))
        }
        ("charUnboundMgr", "GetQAStarterSystemIDs") => {
            let primary_sys = session
                .solar_system_id
                .or_else(|| {
                    session.character_id.and_then(|cid| {
                        ctx.db.with_snapshot(|s| s.characters.get(&cid).map(|c| c.solar_system_id))
                    })
                })
                .or_else(|| {
                    ctx.db.get_characters_for_user(user_id).first().map(|c| c.solar_system_id)
                })
                .unwrap_or(0);
            Some(Ok(PyRep::List(vec![
                PyRep::Integer(primary_sys as i64),
                PyRep::Integer(30000144),
                PyRep::Integer(30002187),
                PyRep::Integer(30002510),
            ])))
        }
        // parity: rpc:charUnboundMgr.ToggleValidation
        ("charUnboundMgr", "ToggleValidation") => handle_toggle_validation(ctx, session, args),
        ("charUnboundMgr", "UpdateCharacterBloodline") => {
            let char_id = match owned_char_arg(ctx, user_id, args, "UpdateCharacterBloodline") {
                Ok(id) => id,
                Err(e) => return Some(Err(e)),
            };
            let bloodline_id = match crate::router::RpcArgs::positional(args).req_u32(1, "bloodlineID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(binfo) = ctx.sde.bloodlines.get(&bloodline_id) else {
                return Some(Err(err("InvalidBloodline", format!("Bloodline {bloodline_id} is invalid"))));
            };
            let is_gm = session.has_gm_role();
            let token_key = user_id.to_string();
            let updated = ctx.db.try_with_mut_snapshot(|snap| {
                let current_race = snap.characters.get(&char_id).map(|c| c.race_id).unwrap_or(0);
                let has_trained_skills = snap.skills.get(&char_id).is_some_and(|m| !m.is_empty());
                let is_pending = snap.cluster_and_runtime.resculpt_pending.contains(&char_id);
                let token_count = snap
                    .cluster_and_runtime
                    .account_resculpt_tokens
                    .get(&token_key)
                    .copied()
                    .unwrap_or(0);
                let resculpt_allowed =
                    is_pending || token_count > 0 || current_race == binfo.race_id || is_gm;
                if !resculpt_allowed {
                    return Err(err("ResculptRequired", "Bloodline change requires active resculpt"));
                }
                if binfo.race_id != current_race && has_trained_skills && !is_gm {
                    return Err(err(
                        "RaceChangeNotAllowed",
                        "Cannot change character empire race after initial character creation",
                    ));
                }
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Ok(false);
                };
                ch.bloodline_id = bloodline_id;
                ch.race_id = binfo.race_id;
                ch.version += 1;
                if !is_gm {
                    if is_pending {
                        snap.cluster_and_runtime.resculpt_pending.remove(&char_id);
                    } else if token_count > 0 {
                        snap.cluster_and_runtime
                            .account_resculpt_tokens
                            .insert(token_key, token_count - 1);
                    }
                }
                Ok(true)
            });
            Some(updated.map(PyRep::Bool))
        }
        ("charUnboundMgr", "UpdateCharacterGender") => {
            let char_id = match owned_char_arg(ctx, user_id, args, "UpdateCharacterGender") {
                Ok(id) => id,
                Err(e) => return Some(Err(e)),
            };
            let gender_id = match crate::router::RpcArgs::positional(args).req_u32(1, "genderID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let updated = ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.gender = gender_id;
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

fn handle_toggle_validation(
    ctx: &ServerContext,
    session: &SessionState,
    _args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let user_id = match ServerContext::require_auth(session) {
        Ok(uid) => uid,
        Err(e) => return Some(Err(e)),
    };
    let next_state = ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let entry = store.validation_toggled_by_user.entry(user_id).or_insert(false);
        *entry = !*entry;
        let next = *entry;
        save_store(snap, &store);
        next
    });
    Some(Ok(PyRep::Bool(next_state)))
}

fn has_active_combat_timer(ctx: &ServerContext, char_id: i64, now: u64) -> bool {
    ctx.crimewatch
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .states
        .get(&char_id)
        .is_some_and(|cw| {
            cw.is_weapon_timer_active(now)
                || cw.is_pvp_active(now)
                || cw.is_criminal(now)
                || now < cw.suspect_flag_until_unix
        })
}

fn validate_character_safe_for_biomass(
    ctx: &ServerContext,
    session: &SessionState,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    now: u64,
    has_combat_timer: bool,
) -> Result<(), BridgeRpcError> {
    let ch = snap.characters.get(&char_id).ok_or_else(|| {
        err(
            "CharacterNotFound",
            format!("Character {char_id} not found"),
        )
    })?;
    if ch.station_id.is_none() && ch.structure_id.is_none() {
        return Err(err(
            "CharacterMustBeDocked",
            "Character must be docked in a station or structure before biomassing",
        ));
    }
    if session.character_id == Some(char_id)
        && session.ship_id.is_some()
        && session.station_id.is_none()
        && session.structure_id.is_none()
    {
        return Err(err(
            "CharacterMustBeDocked",
            "Active character session is undocked in space",
        ));
    }
    if ch.active_ship_id > 0 {
        if let Some(ship_item) = snap.items.get(&ch.active_ship_id) {
            if u32::try_from(ship_item.location_id)
                .ok()
                .is_some_and(|sid| ctx.sde.solar_systems.contains_key(&sid))
            {
                return Err(err(
                    "CharacterMustBeDocked",
                    "Character's active ship is currently undocked in space",
                ));
            }
        }
    }
    if snap
        .sp6_lifecycle
        .disconnected_ships
        .get(&char_id)
        .is_some_and(|d| !d.removed_from_space && now < d.remove_at_unix)
        || snap.sp6_lifecycle.safe_logoffs.contains_key(&char_id)
        || snap
            .sp6_lifecycle
            .self_destructs
            .values()
            .any(|e| e.char_id == char_id)
        || snap.sp6_lifecycle.abyssal_sessions.contains_key(&char_id)
    {
        return Err(err(
            "CharacterInSpace",
            "Character still has an active or disconnecting ship in space",
        ));
    }
    if has_combat_timer {
        return Err(err(
            "CombatTimerActive",
            "Cannot biomass a character while Crimewatch combat or aggression timers are active",
        ));
    }
    Ok(())
}

fn handle_prepare_character_delete(
    ctx: &ServerContext,
    session: &SessionState,
    user_id: u64,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let char_id = owned_char_arg(ctx, user_id, args, "PrepareCharacterForDelete")?;
    let now = ctx.now_unix();
    let has_combat_timer = has_active_combat_timer(ctx, char_id, now);
    ctx.db.try_with_mut_snapshot(|snap| {
        if snap.corporations.values().any(|c| c.ceo_id == char_id) {
            return Err(err(
                "CannotDeleteCorpCEO",
                "A corporation CEO cannot be biomassed; transfer CEO role or dissolve the corporation first",
            ));
        }
        validate_character_safe_for_biomass(ctx, session, snap, char_id, now, has_combat_timer)?;
        let mut store = load_store(snap);
        store.delete_prepared_unix.insert(char_id, now);
        save_store(snap, &store);
        Ok(())
    })?;
    Ok(PyRep::Integer((now + BIOMASS_DELAY_SECS) as i64))
}

fn handle_cancel_character_delete(ctx: &ServerContext, user_id: u64, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let char_id = owned_char_arg(ctx, user_id, args, "CancelCharacterDeletePrepare")?;
    let removed = ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let removed = store.delete_prepared_unix.remove(&char_id).is_some();
        save_store(snap, &store);
        removed
    });
    Ok(PyRep::Bool(removed))
}

fn handle_delete_character(ctx: &ServerContext, session: &mut SessionState, user_id: u64, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let char_id = owned_char_arg(ctx, user_id, args, "DeleteCharacter")?;
    let doomheim = doomheim_corp_id(ctx)
        .ok_or_else(|| err("SdeMissing", "Doomheim corporation not found in SDE"))?;
    let now = ctx.now_unix();
    let is_forced = session.has_gm_role();
    let has_combat_timer = if is_forced {
        false
    } else {
        has_active_combat_timer(ctx, char_id, now)
    };
    let (ch_sys_id, ch_ship_id) = ctx.db.try_with_mut_snapshot(|snap| {
        if snap.corporations.values().any(|c| c.ceo_id == char_id) {
            return Err(err(
                "CannotDeleteCorpCEO",
                "A corporation CEO cannot be biomassed; transfer CEO role or dissolve the corporation first",
            ));
        }
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
        if !is_forced {
            validate_character_safe_for_biomass(ctx, session, snap, char_id, now, has_combat_timer)?;
        }
        let Some(ch_rec) = snap.characters.get(&char_id) else {
            return Err(err(
                "CharacterNotFound",
                format!("Character {char_id} not found"),
            ));
        };
        let sys_and_ship = (ch_rec.solar_system_id, ch_rec.active_ship_id);
        store.delete_prepared_unix.remove(&char_id);
        save_store(snap, &store);
        snap.sp6_lifecycle.disconnected_ships.remove(&char_id);
        snap.sp6_lifecycle.safe_logoffs.remove(&char_id);
        snap.sp6_lifecycle
            .self_destructs
            .retain(|_, e| e.char_id != char_id);
        snap.sp6_lifecycle.abyssal_sessions.remove(&char_id);
        Ok(sys_and_ship)
    })?;
    if ch_ship_id > 0
        && ctx.systems.active_systems_count() > 0
        && tokio::runtime::Handle::try_current().is_ok()
    {
        for (_, h) in ctx.systems.active_system_handles() {
            let mut bp = h.ballpark.write().unwrap_or_else(|e| e.into_inner());
            bp.balls.remove(&ch_ship_id);
            bp.combat_states.remove(&ch_ship_id);
        }
        crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(
            ctx, char_id, ch_ship_id, ch_sys_id,
        );
        crate::services::inventory_fitting::fitting_cyno_portal::on_cyno_ship_destroyed(
            ctx, ch_ship_id,
        );
    }

    // Remove character from all fleets and chat channels (`feat:lifecycle.character-biomass-cleanup`).
    crate::services::fleet_social::fleet_social_notify::on_character_logout(ctx, char_id);
    if session.character_id == Some(char_id) {
        session.character_id = None;
        session.fleet_id = None;
    }

    ctx.db.with_mut_snapshot(|snap| {
        let Some(ch_snap) = snap.characters.get(&char_id).cloned() else {
            return;
        };
        let old_corp_id = ch_snap.corporation_id;
        let char_name = ch_snap.name.clone();
        let home_st = ch_snap
            .station_id
            .or(ch_snap.structure_id)
            .unwrap_or(ch_snap.home_station_id);
        let mut escrow_and_reward_refund = eve_db::IskCents(0);

        // 1. Cancel personal market orders; buy-order escrow and sell-order items follow the asset rule.
        let order_ids: Vec<i64> = snap
            .market_orders
            .values()
            .filter(|o| o.owner_id == char_id && !o.is_npc)
            .map(|o| o.order_id)
            .collect();
        for oid in order_ids {
            if let Some(order) = snap.market_orders.remove(&oid) {
                snap.market_order_meta.remove(&oid);
                if order.vol_remaining > 0 {
                    if order.is_bid {
                        escrow_and_reward_refund = escrow_and_reward_refund
                            .checked_add(order.escrow)
                            .unwrap_or(escrow_and_reward_refund);
                    } else {
                        snap.next_entity_id += 1;
                        let nid = snap.next_entity_id;
                        snap.items.insert(
                            nid,
                            eve_db::InventoryItemRecord {
                                item_id: nid,
                                type_id: order.type_id,
                                owner_id: char_id,
                                location_id: order.station_id,
                                flag: eve_db::InventoryFlag::Hangar,
                                quantity: order.vol_remaining,
                                singleton: false,
                                version: 1,
                                ..Default::default()
                            },
                        );
                    }
                }
            }
        }

        // 2. Remove outstanding contracts and fail in-progress courier contracts accepted by character.
        let contract_ids: Vec<i64> = snap
            .contracts
            .values()
            .filter(|c| {
                !c.for_corp
                    && (c.issuer_id == char_id
                        || c.assignee_id == char_id
                        || c.acceptor_id == Some(char_id)
                        || c.current_bidder_id == Some(char_id))
            })
            .map(|c| c.contract_id)
            .collect();
        for cid in contract_ids {
            let Some(c) = snap.contracts.get(&cid).cloned() else {
                continue;
            };
            if c.status == "Outstanding" {
                if c.issuer_id == char_id {
                    if let Some(bidder_id) = c.current_bidder_id.filter(|&b| b != char_id && c.current_bid.0 > 0) {
                        if let Some(bidder) = snap.characters.get_mut(&bidder_id) {
                            bidder.balance = bidder
                                .balance
                                .checked_add(c.current_bid)
                                .unwrap_or(bidder.balance);
                        }
                    }
                    if c.reward.0 > 0 {
                        escrow_and_reward_refund = escrow_and_reward_refund
                            .checked_add(c.reward)
                            .unwrap_or(escrow_and_reward_refund);
                    }
                    for iid in &c.item_ids {
                        if let Some(item) = snap.items.get_mut(iid) {
                            item.owner_id = char_id;
                            item.location_id = c.start_station_id;
                            item.flag = eve_db::InventoryFlag::Hangar;
                            item.version += 1;
                        }
                    }
                    snap.contracts.remove(&cid);
                } else if c.assignee_id == char_id {
                    if let Some(bidder_id) = c.current_bidder_id.filter(|_| c.current_bid.0 > 0) {
                        if bidder_id == char_id {
                            escrow_and_reward_refund = escrow_and_reward_refund
                                .checked_add(c.current_bid)
                                .unwrap_or(escrow_and_reward_refund);
                        } else if let Some(bidder) = snap.characters.get_mut(&bidder_id) {
                            bidder.balance = bidder
                                .balance
                                .checked_add(c.current_bid)
                                .unwrap_or(bidder.balance);
                        }
                    }
                    if c.reward.0 > 0 {
                        if let Some(issuer) = snap.characters.get_mut(&c.issuer_id) {
                            issuer.balance = issuer
                                .balance
                                .checked_add(c.reward)
                                .unwrap_or(issuer.balance);
                        }
                    }
                    for iid in &c.item_ids {
                        if let Some(item) = snap.items.get_mut(iid) {
                            item.owner_id = c.issuer_id;
                            item.location_id = c.start_station_id;
                            item.flag = eve_db::InventoryFlag::Hangar;
                            item.version += 1;
                        }
                    }
                    if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                        c_mut.status = "Rejected".to_string();
                        c_mut.current_bidder_id = None;
                        c_mut.current_bid = eve_db::IskCents(0);
                    }
                } else if c.current_bidder_id == Some(char_id) {
                    if c.current_bid.0 > 0 {
                        escrow_and_reward_refund = escrow_and_reward_refund
                            .checked_add(c.current_bid)
                            .unwrap_or(escrow_and_reward_refund);
                    }
                    if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                        c_mut.current_bidder_id = None;
                        c_mut.current_bid = eve_db::IskCents(0);
                    }
                }
            } else if c.status == "InProgress" && c.acceptor_id == Some(char_id) {
                snap.fail_courier_contract(cid, now);
                snap.contracts.remove(&cid);
            }
        }

        // 3. Resolve personal industry jobs and planetary colonies (corporate jobs continue `#519`).
        let job_ids: Vec<i64> = snap
            .industry_jobs
            .values()
            .filter(|j| {
                let is_corp_job = crate::services::industry_pi::load_job_cost_meta(snap, j.job_id)
                    .is_some_and(|m| m.use_corp || (m.owner_id > 0 && m.owner_id != char_id))
                    || snap
                        .items
                        .get(&j.blueprint_item_id)
                        .is_some_and(|bp| bp.owner_id != char_id);
                j.installer_id == char_id && !is_corp_job
            })
            .map(|j| j.job_id)
            .collect();
        for jid in job_ids {
            if let Some(job) = snap.industry_jobs.remove(&jid) {
                if let Some(bp) = snap.items.get_mut(&job.blueprint_item_id) {
                    if bp.owner_id == char_id {
                        bp.flag = eve_db::InventoryFlag::Hangar;
                        bp.version += 1;
                    }
                }
            }
        }
        snap.planetary_colonies
            .retain(|&(cid, _), col| cid != char_id && col.owner_id != char_id);
        snap.planetary_colony_layouts
            .retain(|&(cid, _), _| cid != char_id);

        // Ensure character's capsule (type 670) is materialized so asset rules include the capsule.
        let has_capsule = snap
            .items
            .values()
            .any(|i| i.owner_id == char_id && i.type_id == 670);
        if !has_capsule {
            snap.next_entity_id += 1;
            let cap_id = snap.next_entity_id;
            snap.items.insert(
                cap_id,
                eve_db::InventoryItemRecord {
                    item_id: cap_id,
                    type_id: 670,
                    owner_id: char_id,
                    location_id: home_st,
                    flag: eve_db::InventoryFlag::Capsule,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }

        // 4. Remove from corporation membership/roles and apply Player-corp vs NPC-corp economy rules.
        let is_player_corp = snap.corporations.contains_key(&old_corp_id)
            && !ctx.sde.npc_corporations.contains_key(&old_corp_id)
            && old_corp_id != doomheim;
        let total_isk = ch_snap
            .balance
            .checked_add(escrow_and_reward_refund)
            .unwrap_or(ch_snap.balance);

        for corp in snap.corporations.values_mut() {
            corp.members.remove(&char_id);
            corp.member_roles.remove(&char_id);
            corp.applications.retain(|_, app| app.character_id != char_id);
        }
        snap.corp_role_assignments.remove(&(old_corp_id, char_id));
        snap.corp_member_titles.remove(&(old_corp_id, char_id));
        snap.jump_clones.remove(&char_id);
        snap.active_implants.remove(&char_id);

        let ephemeral_char_items: Vec<i64> = snap
            .items
            .values()
            .filter(|i| {
                i.owner_id == char_id
                    && (matches!(
                        i.flag,
                        eve_db::InventoryFlag::Implant
                            | eve_db::InventoryFlag::Booster
                            | eve_db::InventoryFlag::Skill
                    ) || (i.location_id == char_id
                        && i.type_id != 670
                        && i.flag != eve_db::InventoryFlag::Capsule)
                        || i.type_id == 3468)
            })
            .map(|i| i.item_id)
            .collect();
        for iid in ephemeral_char_items {
            snap.items.remove(&iid);
        }

        let mut locked_contract_items: std::collections::HashSet<i64> = snap
            .items
            .values()
            .filter(|i| {
                i.flag == eve_db::InventoryFlag::Locked
                    || snap.is_item_locked_in_contract_or_job(i.item_id)
            })
            .map(|i| i.item_id)
            .collect();
        let child_locked: Vec<i64> = snap
            .items
            .values()
            .filter(|i| {
                let mut cur = i.location_id;
                let mut steps = 0;
                while steps < 16 {
                    if locked_contract_items.contains(&cur) {
                        return true;
                    }
                    let Some(parent) = snap.items.get(&cur) else {
                        break;
                    };
                    cur = parent.location_id;
                    steps += 1;
                }
                false
            })
            .map(|i| i.item_id)
            .collect();
        locked_contract_items.extend(child_locked);

        let mut struct_ext =
            crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        let char_wrap_ids: std::collections::HashSet<i64> = struct_ext
            .asset_safety_wraps
            .values()
            .filter(|w| !w.is_corp && w.owner_id == char_id)
            .map(|w| w.wrap_id)
            .collect();
        // Items inside active Asset Safety wraps (`flag == Locked`) belong to `char_id` and must
        // NOT be misclassified as third-party contract items (`#519`).
        for wid in &char_wrap_ids {
            locked_contract_items.remove(wid);
        }
        locked_contract_items.retain(|iid| {
            snap.items
                .get(iid)
                .is_none_or(|it| !char_wrap_ids.contains(&it.location_id))
        });

        if is_player_corp {
            let mut office_stations: Vec<i64> = snap
                .items
                .values()
                .filter(|i| i.owner_id == old_corp_id && i.type_id == 27)
                .map(|i| i.location_id)
                .collect();
            office_stations.sort_unstable();
            if office_stations.is_empty() {
                // No corporate office: all character assets and asset-safety wraps are destroyed.
                for wid in &char_wrap_ids {
                    struct_ext.asset_safety_wraps.remove(wid);
                }
                snap.items
                    .retain(|id, i| i.owner_id != char_id || locked_contract_items.contains(id));
            } else {
                for wid in &char_wrap_ids {
                    if let Some(w) = struct_ext.asset_safety_wraps.get_mut(wid) {
                        w.owner_id = old_corp_id;
                        w.is_corp = true;
                    }
                }
                let owned_ids: std::collections::HashSet<i64> = snap
                    .items
                    .values()
                    .filter(|i| i.owner_id == char_id && !locked_contract_items.contains(&i.item_id))
                    .map(|i| i.item_id)
                    .collect();
                let mut transferred_ids = Vec::new();
                for item in snap.items.values_mut() {
                    if item.owner_id == char_id && !locked_contract_items.contains(&item.item_id) {
                        if item.owner_id != old_corp_id {
                            transferred_ids.push(item.item_id);
                        }
                        item.owner_id = old_corp_id;
                        if char_wrap_ids.contains(&item.item_id)
                            || char_wrap_ids.contains(&item.location_id)
                        {
                            // Preserve Asset Safety wrap and wrapped contents at their exact
                            // `location_id` and `flag` without bypassing retrieval fees (`#519`).
                        } else if !owned_ids.contains(&item.location_id) {
                            // Keep items at their existing `location_id` without cross-universe
                            // teleportation (`#519`).
                            if office_stations.contains(&item.location_id) {
                                item.flag = eve_db::InventoryFlag::CorpSAG1;
                            } else if !u32::try_from(item.location_id)
                                .ok()
                                .is_some_and(|sid| ctx.sde.solar_systems.contains_key(&sid))
                            {
                                item.flag = eve_db::InventoryFlag::Impounded;
                            }
                        }
                        item.version += 1;
                    }
                }
                snap.strip_ship_cosmetics(&transferred_ids);
            }
            if !char_wrap_ids.is_empty() {
                crate::services::station_structure::structure_upwell_ops::save_ext_state(
                    snap,
                    &struct_ext,
                );
            }

            let mut remaining_members = Vec::new();
            let mut corp_name = String::from("Corporation");
            let mut new_div1_bal = eve_db::IskCents(0);
            if let Some(corp) = snap.corporations.get_mut(&old_corp_id) {
                corp_name = corp.name.clone();
                remaining_members = corp.members.iter().copied().collect();
                remaining_members.sort_unstable();
                if total_isk.0 > 0 {
                    corp.ensure_wallet_divisions();
                    let cur = corp.division_balance(1);
                    new_div1_bal = cur.checked_add(total_isk).unwrap_or(cur);
                    corp.set_division_balance(1, new_div1_bal);
                }
            }
            if total_isk.0 > 0 {
                let desc = format!("Inheritance from biomassed character {char_name}");
                let jid = snap.append_journal_entry(eve_db::ledger::JournalEntryInput {
                    owner_id: old_corp_id,
                    entry_type: "Inheritance",
                    amount: total_isk,
                    balance_after: new_div1_bal,
                    reason: desc.clone(),
                    meta: eve_db::ledger::JournalMeta::new(9, char_id, old_corp_id, now)
                        .with_context(eve_db::ledger::JournalContext::Character, char_id)
                        .with_account_key(1000),
                });
                snap.wallet_journal_timestamps.insert(jid, now);
                snap.corp_wallet_journals
                    .entry((old_corp_id, 1))
                    .or_default()
                    .push(eve_db::CorpWalletJournalRecord {
                        id: jid,
                        date_unix: now,
                        ref_type: "Inheritance".into(),
                        description: desc.clone(),
                        amount: total_isk.to_wire_f64(),
                        balance: new_div1_bal.to_wire_f64(),
                        first_party_id: Some(char_id),
                        second_party_id: Some(old_corp_id),
                        reason: Some(desc),
                    });
            }
            for member_id in remaining_members {
                snap.next_entity_id += 1;
                let max_mid = snap.mails.iter().map(|m| m.message_id).max().unwrap_or(9_000_000);
                let mid = snap.next_entity_id.max(max_mid + 1);
                snap.next_entity_id = snap.next_entity_id.max(mid);
                snap.mails.push(eve_db::MailRecord {
                    message_id: mid,
                    sender_id: char_id,
                    recipient_id: member_id,
                    subject: format!("Inheritance: {char_name} biomassed"),
                    body: format!(
                        "{char_name} has been biomassed into Doomheim. Assets and wallet ISK have been transferred to {corp_name}."
                    ),
                });
                snap.mail_timestamps.insert(mid, now);
            }
        } else {
            // NPC corporation member: all assets, asset-safety wraps, and wallet ISK are destroyed.
            if !char_wrap_ids.is_empty() {
                for wid in &char_wrap_ids {
                    struct_ext.asset_safety_wraps.remove(wid);
                }
                crate::services::station_structure::structure_upwell_ops::save_ext_state(
                    snap,
                    &struct_ext,
                );
            }
            snap.items
                .retain(|id, i| i.owner_id != char_id || locked_contract_items.contains(id));
        }

        if let Some(ch) = snap.characters.get_mut(&char_id) {
            // Biomassed: the character leaves the account (freeing the slot) and joins Doomheim.
            ch.user_id = 0;
            ch.corporation_id = doomheim;
            ch.alliance_id = None;
            ch.war_faction_id = None;
            ch.balance = eve_db::IskCents(0);
            ch.version += 1;
        }
    });
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
    kwargs: &HashMap<String, PyRep>,
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
            let (station_id, type_id, solar_system_id) = ctx.db.with_snapshot(|snap| {
                let st_id = snap.characters
                    .get(&char_id)
                    .map(|c| c.home_station_id)
                    .unwrap_or(0);
                if let Some(st) = ctx.sde.stations.get(&st_id) {
                    (st_id, st.station_type_id as i64, st.solar_system_id as i64)
                } else if let Some(st) = snap.structures.get(&st_id) {
                    (st_id, st.type_id as i64, st.solar_system_id as i64)
                } else {
                    // Look up character's school / starter station from snap.characters / SDE bloodlines
                    let (school_id, bloodline_id) = snap
                        .characters
                        .get(&char_id)
                        .map(|c| (c.school_id, c.bloodline_id))
                        .unwrap_or((0, 0));
                    let starter_st = ctx
                        .sde
                        .schools
                        .get(&school_id)
                        .and_then(|s| ctx.sde.stations.get(&s.station_id))
                        .or_else(|| {
                            let corp_id = ctx.sde.bloodlines.get(&bloodline_id).map(|b| b.corporation_id);
                            ctx.sde.stations.values().find(|st| Some(st.owner_corporation_id) == corp_id)
                        })
                        .or_else(|| ctx.sde.stations.values().min_by_key(|st| st.station_id));
                    if let Some(st) = starter_st {
                        (st.station_id, st.station_type_id as i64, st.solar_system_id as i64)
                    } else {
                        (st_id, 0, 0)
                    }
                }
            });
            Some(Ok(PyRep::keyval([
                ("stationID", PyRep::Integer(station_id)),
                ("typeID", PyRep::Integer(type_id)),
                ("stationTypeID", PyRep::Integer(type_id)),
                ("solarSystemID", PyRep::Integer(solar_system_id)),
            ])))
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
                        let wf = ch.and_then(|c| c.war_faction_id);
                        (
                            PyRep::Integer(cid),
                            PyRep::keyval([
                                ("characterID", PyRep::Integer(cid)),
                                ("corporationID", PyRep::Integer(corp)),
                                ("allianceID", ally.map(PyRep::Integer).unwrap_or(PyRep::None)),
                                ("warFactionID", wf.map(PyRep::Integer).unwrap_or(PyRep::None)),
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
            if char_id != session_char_id {
                let authorized = ctx.db.with_snapshot(|snap| {
                    let target_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                    let caller_corp = snap.characters.get(&session_char_id).map(|c| c.corporation_id).unwrap_or(0);
                    target_corp > 0 && target_corp == caller_corp && has_corp_director_or_personnel_manager_role(snap, session_char_id, target_corp)
                });
                if !authorized {
                    return Some(Err(err(
                        "CustomError",
                        "Access denied: viewing private corp change info requires Director or Personnel Manager role",
                    )));
                }
            }
            let (corp_id, roles, corp_join_ft) = ctx.db.with_snapshot(|snap| {
                let (corp_id, create_unix) = snap.characters
                    .get(&char_id)
                    .map(|c| (c.corporation_id, c.create_date_unix))
                    .unwrap_or((1000044, 0));
                let start_unix = snap
                    .employment_history
                    .get(&char_id)
                    .and_then(|h| h.iter().rev().find(|e| e.0 == corp_id).map(|e| e.1))
                    .unwrap_or(create_unix);
                let corp_join_ft = if start_unix > 0 {
                    unix_to_filetime(start_unix)
                } else {
                    ctx.now_blueos_filetime_ticks()
                };
                let (_, mut role_mask) = snap.effective_corp_roles_at(
                    corp_id,
                    char_id,
                    None,
                    0,
                    ctx.now_unix(),
                );
                if let Some(scoped) = snap.char_scoped_roles.get(&char_id) {
                    role_mask |= eve_db::corp_roles::encode_corp_role_names(&scoped.roles);
                }
                (corp_id, role_mask, corp_join_ft)
            });
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                ("corporationID", PyRep::Integer(corp_id)),
                ("roles", PyRep::Integer(roles as i64)),
                ("corporationDateTime", PyRep::Integer(corp_join_ft)),
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
                        c.race_id,
                        c.bloodline_id,
                        c.gender,
                        c.school_id,
                    )
                })
            });
            let (name, corp, ally, sec, desc, cdate, race, bloodline, gender, school) = info.unwrap_or((
                "Unknown".into(),
                1000044,
                None,
                0.0,
                String::new(),
                ctx.now_blueos_filetime_ticks(),
                1,
                1,
                0,
                0,
            ));
            let (war_fac, rank) = ctx.db.with_snapshot(|snap| {
                snap.characters.get(&char_id).and_then(|ch| ch.war_faction_id).map(|wfid| {
                    let st = snap.characters.get(&char_id).and_then(|c| c.standings.get(&wfid).copied()).unwrap_or(0.0);
                    (Some(wfid), crate::services::fw_services::calculate_militia_rank(st))
                }).unwrap_or((None, 0))
            });
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                ("characterName", PyRep::String(name)),
                ("corporationID", PyRep::Integer(corp)),
                ("allianceID", ally.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("securityStatus", PyRep::Real(sec)),
                ("description", PyRep::String(desc)),
                ("createDateTime", PyRep::Integer(cdate)),
                ("raceID", PyRep::Integer(race as i64)),
                ("bloodlineID", PyRep::Integer(bloodline as i64)),
                ("gender", PyRep::Integer(gender as i64)),
                ("schoolID", PyRep::Integer(school as i64)),
                ("warFactionID", war_fac.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("militiaRank", PyRep::Integer(rank)),
                ("rank", PyRep::Integer(rank)),
            ])))
        }
        "GetRecentShipKillsAndLosses" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = resolve_combat_log_entries(ctx, |km| {
                km.victim_character_id == char_id || km.attacker_character_id == char_id
            })
            .into_iter()
            .map(|e| {
                vec![
                    PyRep::Integer(e.kill_id),
                    PyRep::Integer(e.victim_character_id),
                    PyRep::Integer(e.solar_system_id as i64),
                    PyRep::Integer(e.killed_unix as i64),
                    PyRep::None,
                    PyRep::Integer(e.victim_character_id),
                    PyRep::Integer(e.victim_corp_id),
                    e.victim_alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    PyRep::None,
                    PyRep::Integer(e.victim_ship_type_id as i64),
                    PyRep::Integer(e.victim_damage_taken),
                    PyRep::Integer(e.final_character_id),
                    PyRep::Integer(e.final_corp_id),
                    e.final_alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    PyRep::None,
                    e.final_ship_type_id.map(|t| PyRep::Integer(t as i64)).unwrap_or(PyRep::None),
                    e.final_weapon_type_id.map(|t| PyRep::Integer(t as i64)).unwrap_or(PyRep::None),
                    PyRep::Real(e.final_security_status),
                    PyRep::Integer(e.final_damage_done),
                    PyRep::String(String::new()),
                ]
            })
            .collect();
            Some(Ok(PyRep::Rowset {
                columns: vec![
                    "killID".into(),
                    "victimID".into(),
                    "solarSystemID".into(),
                    "killTime".into(),
                    "moonID".into(),
                    "victimCharacterID".into(),
                    "victimCorporationID".into(),
                    "victimAllianceID".into(),
                    "victimFactionID".into(),
                    "victimShipTypeID".into(),
                    "victimDamageTaken".into(),
                    "finalCharacterID".into(),
                    "finalCorporationID".into(),
                    "finalAllianceID".into(),
                    "finalFactionID".into(),
                    "finalShipTypeID".into(),
                    "finalWeaponTypeID".into(),
                    "finalSecurityStatus".into(),
                    "finalDamageDone".into(),
                    "killBlob".into(),
                ],
                rows,
            }))
        }
        "SetActivityStatus" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let status = kwargs
                .get("status")
                .or_else(|| args.first())
                .and_then(|v| v.as_str().map(|s| s.to_string()).or_else(|| v.as_i64().map(|n| n.to_string())));
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.activity_status = status;
                    ch.version = ch.version.wrapping_add(1);
                }
            });
            Some(Ok(PyRep::None))
        }
        "GetCohortsForCharacter" => {
            let (_, session_char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let char_id = args.first().and_then(|a| a.as_i64()).unwrap_or(session_char_id);
            if char_id != session_char_id {
                return Some(Err(err(
                    "CustomError",
                    "Access denied: cannot query cohorts for another character",
                )));
            }
            let cohorts = ctx.db.with_snapshot(|snap| {
                if let Some(list) = snap.cluster_and_runtime.char_cohorts.get(&char_id) {
                    return list.iter().cloned().map(PyRep::String).collect();
                }
                let mut derived = Vec::new();
                if let Some(ch) = snap.characters.get(&char_id) {
                    let year = 1970 + (ch.create_date_unix / 31_536_000);
                    derived.push(PyRep::String(format!("cohort_created_{year}")));
                    let age_secs = ctx.now_unix().saturating_sub(ch.create_date_unix);
                    if age_secs < 30 * 86_400 {
                        derived.push(PyRep::String("cohort_new_player".into()));
                    } else {
                        derived.push(PyRep::String("cohort_veteran".into()));
                    }
                    if ch.corporation_id > 0 {
                        derived.push(PyRep::String(format!("cohort_corp_{}", ch.corporation_id)));
                    }
                } else {
                    derived.push(PyRep::String("cohort_default".into()));
                }
                derived
            });
            Some(Ok(PyRep::List(cohorts)))
        }
        _ => None,
    }
}

pub(crate) struct CombatLogResolvedEntry {
    pub kill_id: i64,
    pub victim_character_id: i64,
    pub victim_corp_id: i64,
    pub victim_alliance_id: Option<i64>,
    pub victim_ship_type_id: u32,
    pub victim_damage_taken: i64,
    pub solar_system_id: u32,
    pub killed_unix: u64,
    pub final_character_id: i64,
    pub final_corp_id: i64,
    pub final_alliance_id: Option<i64>,
    pub final_ship_type_id: Option<u32>,
    pub final_weapon_type_id: Option<u32>,
    pub final_security_status: f64,
    pub final_damage_done: i64,
}

pub(crate) fn resolve_combat_log_entries(
    ctx: &ServerContext,
    filter: impl Fn(&eve_db::KillmailRecord) -> bool,
) -> Vec<CombatLogResolvedEntry> {
    let candidates: Vec<_> = ctx.db.with_snapshot(|snap| {
        snap.killmails
            .values()
            .filter(|km| filter(km))
            .map(|km| {
                let snap_dkm = snap
                    .combat_crimewatch_state
                    .detailed_killmails
                    .get(&km.kill_id)
                    .cloned();
                let victim_corp_id = if km.victim_corp_id > 0 {
                    km.victim_corp_id
                } else {
                    snap.characters
                        .get(&km.victim_character_id)
                        .map(|c| c.corporation_id)
                        .unwrap_or(0)
                };
                let victim_alliance_id = km.victim_alliance_id.or_else(|| {
                    snap.corporations
                        .get(&victim_corp_id)
                        .and_then(|c| c.alliance_id)
                });
                let att_corp_id = if km.attacker_corp_id > 0 {
                    km.attacker_corp_id
                } else {
                    snap.characters
                        .get(&km.attacker_character_id)
                        .map(|c| c.corporation_id)
                        .unwrap_or(0)
                };
                let att_alliance_id = km.attacker_alliance_id.or_else(|| {
                    snap.corporations
                        .get(&att_corp_id)
                        .and_then(|c| c.alliance_id)
                });
                (
                    km.clone(),
                    snap_dkm,
                    victim_corp_id,
                    victim_alliance_id,
                    att_corp_id,
                    att_alliance_id,
                )
            })
            .collect()
    });
    if candidates.is_empty() {
        return Vec::new();
    }
    let live_dkms: std::collections::HashMap<i64, eve_db::DetailedKillmail> =
        crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            candidates
                .iter()
                .filter_map(|(km, _, _, _, _, _)| {
                    rt.detailed_killmails
                        .get(&km.kill_id)
                        .cloned()
                        .map(|d| (km.kill_id, d))
                })
                .collect()
        });
    let mut entries: Vec<CombatLogResolvedEntry> = candidates
        .into_iter()
        .map(
            |(km, snap_dkm, victim_corp_id, victim_alliance_id, att_corp_id, att_alliance_id)| {
                let dkm = live_dkms.get(&km.kill_id).cloned().or(snap_dkm);
                let final_attacker = dkm.as_ref().and_then(|d| {
                    d.attackers
                        .iter()
                        .find(|a| a.final_blow)
                        .or_else(|| d.attackers.first())
                });
                let victim_damage_taken = dkm
                    .as_ref()
                    .map(|d| d.victim_damage_taken.max(0.0).round() as i64)
                    .unwrap_or(0);
                let final_character_id = final_attacker
                    .map(|a| a.character_id)
                    .filter(|&id| id > 0)
                    .unwrap_or(km.attacker_character_id);
                let final_corp_id = final_attacker
                    .map(|a| a.corporation_id)
                    .filter(|&id| id > 0)
                    .unwrap_or(att_corp_id);
                let final_alliance_id = final_attacker
                    .and_then(|a| a.alliance_id)
                    .or(att_alliance_id);
                let final_ship_type_id =
                    final_attacker.map(|a| a.ship_type_id).filter(|&t| t > 0);
                let final_weapon_type_id =
                    final_attacker.map(|a| a.weapon_type_id).filter(|&t| t > 0);
                let final_security_status =
                    final_attacker.map(|a| a.security_status).unwrap_or(0.0);
                let final_damage_done = final_attacker
                    .map(|a| a.damage_done.max(0.0).round() as i64)
                    .unwrap_or(0);
                CombatLogResolvedEntry {
                    kill_id: km.kill_id,
                    victim_character_id: km.victim_character_id,
                    victim_corp_id,
                    victim_alliance_id,
                    victim_ship_type_id: km.ship_type_id,
                    victim_damage_taken,
                    solar_system_id: km.solar_system_id,
                    killed_unix: km.killed_unix,
                    final_character_id,
                    final_corp_id,
                    final_alliance_id,
                    final_ship_type_id,
                    final_weapon_type_id,
                    final_security_status,
                    final_damage_done,
                }
            },
        )
        .collect();
    entries.sort_by_key(|e| (std::cmp::Reverse(e.killed_unix), std::cmp::Reverse(e.kill_id)));
    entries
}

#[cfg(test)]
#[path = "character_lifecycle_tests.rs"]
mod character_lifecycle_tests;

