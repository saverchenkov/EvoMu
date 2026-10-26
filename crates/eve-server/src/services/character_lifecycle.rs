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
}

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
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
    if let Some(corp) = snap.corporations.get(&target_corp_id) {
        if corp.ceo_id == caller_char_id {
            return true;
        }
        let member_role = corp.member_roles.get(&caller_char_id).copied().unwrap_or(0);
        let assigned_role = snap.corp_role_assignments.get(&(target_corp_id, caller_char_id)).map(|ra| ra.roles).unwrap_or(0);
        let mut total_roles = member_role | assigned_role;
        if let Some(scoped) = snap.char_scoped_roles.get(&caller_char_id) {
            for r_name in &scoped.roles {
                if r_name == "Director" {
                    total_roles |= eve_db::corp_roles::ROLE_DIRECTOR;
                } else if r_name == "Personnel_Manager" {
                    total_roles |= eve_db::corp_roles::ROLE_PERSONNEL_MANAGER;
                }
            }
        }
        (total_roles & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_PERSONNEL_MANAGER)) != 0
    } else {
        false
    }
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
            let char_id = args.first().and_then(|a| a.as_i64())?;
            let lock = ctx.db.with_snapshot(|s| {
                if let Some(blob) = s.service_state_blobs.get(&format!("character_lock:{char_id}")) {
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
        ("charUnboundMgr", "PrepareCharacterForDelete") => Some(prepare(ctx, user_id, args)),
        ("charUnboundMgr", "CancelCharacterDeletePrepare") => Some(cancel(ctx, user_id, args)),
        ("charUnboundMgr", "DeleteCharacter") => Some(delete(ctx, session, user_id, args)),
        ("charUnboundMgr", "GetQAStarterSystemIDs") => {
            Some(Ok(PyRep::List(vec![
                PyRep::Integer(30000142),
                PyRep::Integer(30000144),
                PyRep::Integer(30002187),
                PyRep::Integer(30002510),
            ])))
        }
        ("charUnboundMgr", "ToggleValidation") => {
            // const-ok: toggle validation confirmation for character creation pipeline
            Some(Ok(PyRep::Bool(true)))
        }
        ("charUnboundMgr", "UpdateCharacterBloodline") => {
            let char_id = match owned_char_arg(ctx, user_id, args, "UpdateCharacterBloodline") {
                Ok(id) => id,
                Err(e) => return Some(Err(e)),
            };
            let bloodline_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1) as u32;
            if !ctx.sde.bloodlines.contains_key(&bloodline_id) {
                return Some(Err(err("InvalidBloodline", format!("Bloodline {bloodline_id} is invalid"))));
            }
            let resculpt_allowed = ctx.db.with_snapshot(|snap| {
                let is_pending = snap.service_state_blobs.get(&format!("resculpt_pending:{char_id}")).is_some_and(|v| v == "true");
                let has_tokens = snap.service_state_blobs.get(&format!("account_resculpt_tokens:{user_id}")).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0) > 0;
                let ch_race = snap.characters.get(&char_id).map(|c| c.race_id).unwrap_or(0);
                let target_race = ctx.sde.bloodlines.get(&bloodline_id).map(|b| b.race_id).unwrap_or(0);
                is_pending || has_tokens || ch_race == target_race || session.has_gm_role()
            });
            if !resculpt_allowed {
                return Some(Err(err("ResculptRequired", "Bloodline change requires active resculpt")));
            }
            let updated = ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.bloodline_id = bloodline_id;
                    if let Some(binfo) = ctx.sde.bloodlines.get(&bloodline_id) {
                        ch.race_id = binfo.race_id;
                    }
                    ch.version += 1;
                    true
                } else {
                    false
                }
            });
            Some(Ok(PyRep::Bool(updated)))
        }
        ("charUnboundMgr", "UpdateCharacterGender") => {
            let char_id = match owned_char_arg(ctx, user_id, args, "UpdateCharacterGender") {
                Ok(id) => id,
                Err(e) => return Some(Err(e)),
            };
            let gender_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let updated = ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.gender = gender_id as u32;
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

fn prepare(ctx: &ServerContext, user_id: u64, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let char_id = owned_char_arg(ctx, user_id, args, "PrepareCharacterForDelete")?;
    let now = ctx.now_unix();
    ctx.db.try_with_mut_snapshot(|snap| {
        if snap.corporations.values().any(|c| c.ceo_id == char_id) {
            return Err(err(
                "CannotDeleteCorpCEO",
                "A corporation CEO cannot be biomassed; transfer CEO role or dissolve the corporation first",
            ));
        }
        let mut store = load_store(snap);
        store.delete_prepared_unix.insert(char_id, now);
        save_store(snap, &store);
        Ok(())
    })?;
    Ok(PyRep::Integer((now + BIOMASS_DELAY_SECS) as i64))
}

fn cancel(ctx: &ServerContext, user_id: u64, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let char_id = owned_char_arg(ctx, user_id, args, "CancelCharacterDeletePrepare")?;
    let removed = ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let removed = store.delete_prepared_unix.remove(&char_id).is_some();
        save_store(snap, &store);
        removed
    });
    Ok(PyRep::Bool(removed))
}

fn delete(ctx: &ServerContext, session: &mut SessionState, user_id: u64, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let char_id = owned_char_arg(ctx, user_id, args, "DeleteCharacter")?;
    let doomheim = doomheim_corp_id(ctx)
        .ok_or_else(|| err("SdeMissing", "Doomheim corporation not found in SDE"))?;
    let now = ctx.now_unix();
    let is_forced = session.has_gm_role();
    ctx.db.try_with_mut_snapshot(|snap| {
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
        store.delete_prepared_unix.remove(&char_id);
        save_store(snap, &store);
        if !snap.characters.contains_key(&char_id) {
            return Err(err(
                "CharacterNotFound",
                format!("Character {char_id} not found"),
            ));
        }
        Ok(())
    })?;

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
        let home_st = ch_snap.station_id.unwrap_or(ch_snap.home_station_id);
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

        // 3. Resolve industry jobs and planetary colonies.
        let job_ids: Vec<i64> = snap
            .industry_jobs
            .values()
            .filter(|j| j.installer_id == char_id)
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

        if is_player_corp {
            let office_stations: Vec<i64> = snap
                .items
                .values()
                .filter(|i| i.owner_id == old_corp_id && i.type_id == 27)
                .map(|i| i.location_id)
                .collect();
            if office_stations.is_empty() {
                // No corporate office: all character assets are destroyed.
                snap.items.retain(|_, i| i.owner_id != char_id);
            } else {
                let owned_ids: std::collections::HashSet<i64> = snap
                    .items
                    .values()
                    .filter(|i| i.owner_id == char_id)
                    .map(|i| i.item_id)
                    .collect();
                for item in snap.items.values_mut() {
                    if item.owner_id == char_id {
                        item.owner_id = old_corp_id;
                        if !owned_ids.contains(&item.location_id) {
                            let target_st = if office_stations.contains(&item.location_id) {
                                item.location_id
                            } else {
                                office_stations[0]
                            };
                            item.location_id = target_st;
                            item.flag = eve_db::InventoryFlag::CorpSAG1;
                        }
                        item.version += 1;
                    }
                }
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
            // NPC corporation member: all assets and wallet ISK are destroyed.
            snap.items.retain(|_, i| i.owner_id != char_id);
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
                        (
                            PyRep::Integer(cid),
                            PyRep::keyval([
                                ("characterID", PyRep::Integer(cid)),
                                ("corporationID", PyRep::Integer(corp)),
                                ("allianceID", ally.map(PyRep::Integer).unwrap_or(PyRep::None)),
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
                let mut role_mask = snap.corporations.get(&corp_id).map(|c| {
                    if c.ceo_id == char_id {
                        u64::MAX
                    } else {
                        c.member_roles.get(&char_id).copied().unwrap_or(0)
                    }
                }).unwrap_or(0);
                if let Some(ra) = snap.corp_role_assignments.get(&(corp_id, char_id)) {
                    role_mask |= ra.roles;
                }
                if let Some(scoped) = snap.char_scoped_roles.get(&char_id) {
                    for r_name in &scoped.roles {
                        let bit = match r_name.as_str() {
                            "Director" => eve_db::corp_roles::ROLE_DIRECTOR,
                            "Personnel_Manager" => eve_db::corp_roles::ROLE_PERSONNEL_MANAGER,
                            "Accountant" => eve_db::corp_roles::ROLE_ACCOUNTANT,
                            "Security_Officer" => eve_db::corp_roles::ROLE_SECURITY_OFFICER,
                            "Factory_Manager" => eve_db::corp_roles::ROLE_FACTORY_MANAGER,
                            "Station_Manager" => eve_db::corp_roles::ROLE_STATION_MANAGER,
                            "Auditor" => eve_db::corp_roles::ROLE_AUDITOR,
                            "Diplomat" => eve_db::corp_roles::ROLE_DIPLOMAT,
                            "Config_Equipment" | "Equipment_Config" => eve_db::corp_roles::ROLE_CONFIG_EQUIPMENT,
                            "Starbase_Config" => eve_db::corp_roles::ROLE_STARBASE_CONFIG,
                            "Starbase_Operator" => eve_db::corp_roles::ROLE_STARBASE_OPERATOR,
                            "Junior_Accountant" => eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT,
                            "Trader" => eve_db::corp_roles::ROLE_TRADER,
                            "Contract_Manager" => eve_db::corp_roles::ROLE_CONTRACT_MANAGER,
                            "Project_Manager" => eve_db::corp_roles::ROLE_PROJECT_MANAGER,
                            _ => 0,
                        };
                        role_mask |= bit;
                    }
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
                        c.bloodline_id,
                        c.gender,
                        c.school_id,
                    )
                })
            });
            let (name, corp, ally, sec, desc, cdate, bloodline, gender, school) = info.unwrap_or((
                "Unknown".into(),
                1000044,
                None,
                0.0,
                String::new(),
                ctx.now_blueos_filetime_ticks(),
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
            let rows = ctx.db.with_snapshot(|snap| {
                snap.killmails
                    .values()
                    .filter(|km| km.victim_character_id == char_id || km.attacker_character_id == char_id)
                    .map(|km| vec![
                        PyRep::Integer(km.kill_id),
                        PyRep::Integer(km.victim_character_id),
                        PyRep::Integer(km.solar_system_id as i64),
                        PyRep::Integer(km.killed_unix as i64),
                    ])
                    .collect()
            });
            Some(Ok(PyRep::Rowset {
                columns: vec!["killID".into(), "victimID".into(), "solarSystemID".into(), "killTime".into()],
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
                let blob_key = format!("char_cohorts:{char_id}");
                if let Some(json_str) = snap.service_state_blobs.get(&blob_key) {
                    if let Ok(list) = serde_json::from_str::<Vec<String>>(json_str) {
                        return list.into_iter().map(PyRep::String).collect();
                    }
                }
                let mut derived = Vec::new();
                if let Some(ch) = snap.characters.get(&char_id) {
                    let year = 1970 + (ch.create_date_unix / 31_536_000);
                    derived.push(PyRep::String(format!("cohort_created_{year}")));
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
        "SetSessionCheck" | "clear_memoized" => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::Bool(true)))
        }
        _ => None,
    }
}
