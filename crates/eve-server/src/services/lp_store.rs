//! `lp_store` — Loyalty Point (LP) Store & LP Wallet Service (`E-5` / `E-6`).
//!
//! Handles `LPStoreMgr`, `LPSvc`, and `rewardMgr` RPCs for:
//! - Looking up available corporation LP store offerings (`GetAvailableOffersFromCorp`)
//! - Exchanging LP, ISK, and required items (`TakeOfferForCharacter`, `TakeOfferForCorporation`)
//! - Querying character and corporation LP balances (`GetAllMyCharacterWalletLPBalances`, `GetAllMyCorporationWalletLPBalances`)
//! - Transferring LP to corporations (`TransferLPFromMyWalletToOtherCorp`, `TransferLPFromMyCorpWalletToOtherCorp`)
//! - Converting CONCORD LP to empire corporation LP (`ExchangeConcordLP`)
//! - Querying character LP reward logs (`GetRewardLPLogs`)

// parity: plan:E-5, plan:E-6
// parity: feat:lp.lp-balance
// parity: feat:lp.lp-corp-wallet, feat:lp.concord-lp, feat:lp.lp-reward-logs, feat:wallet.evermarks
// parity: feat:lifecycle.lp-durable
// parity: rpc:LPStoreMgr.GetAvailableOffersFromCorp
// parity: rpc:LPStoreMgr.TakeOfferForCharacter, rpc:LPStoreMgr.TakeOfferForCorporation
// parity: feat:lp.lp-store-offers, feat:lp.lp-store-buy

#![forbid(unsafe_code)]

use crate::router::ServerContext;
pub use eve_db::LpRewardLogEntry;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const CONCORD_CORP_ID: i64 = 1_000_125;
pub const PARAGON_CORP_ID: i64 = 1_000_419;
const LP_STORE_OFFERS_KEY: &str = "lp_store_offers";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LPOffer {
    pub offer_id: i64,
    pub corp_id: i64,
    pub type_id: TypeId,
    pub quantity: i32,
    pub lp_cost: i64,
    pub isk_cost: IskCents,
    pub req_items: Vec<(TypeId, i32)>,
    #[serde(default)]
    pub ak_cost: i64,
}

/// Load persisted LP store offers from `WorldStateSnapshot.service_state_blobs["lp_store_offers"]`.
///
/// DATA GAP: LP store corporation offer catalog is server-only and not present in V24 client FSD.
pub fn load_lp_store_offers(snap: &eve_db::WorldStateSnapshot) -> Vec<LPOffer> {
    snap.service_state_blobs
        .get(LP_STORE_OFFERS_KEY)
        .and_then(|json| serde_json::from_str::<Vec<LPOffer>>(json).ok())
        .unwrap_or_default()
}

pub fn save_lp_store_offers(snap: &mut eve_db::WorldStateSnapshot, offers: &[LPOffer]) {
    if let Ok(json) = serde_json::to_string(offers) {
        snap.service_state_blobs
            .insert(LP_STORE_OFFERS_KEY.to_string(), json);
    }
}

pub fn register_lp_store_offer(snap: &mut eve_db::WorldStateSnapshot, offer: LPOffer) {
    let mut offers = load_lp_store_offers(snap);
    if let Some(existing) = offers
        .iter_mut()
        .find(|o| o.corp_id == offer.corp_id && o.offer_id == offer.offer_id)
    {
        *existing = offer;
    } else {
        offers.push(offer);
    }
    save_lp_store_offers(snap, &offers);
}

pub fn load_corp_lp_balances(snap: &eve_db::WorldStateSnapshot, corp_id: i64) -> HashMap<i64, i64> {
    snap.corp_lp_balances
        .iter()
        .filter_map(|(&(cid, issuer_id), &lp)| (cid == corp_id).then_some((issuer_id, lp)))
        .collect()
}

pub fn save_corp_lp_balances(
    snap: &mut eve_db::WorldStateSnapshot,
    corp_id: i64,
    balances: &HashMap<i64, i64>,
) {
    snap.corp_lp_balances.retain(|&(cid, _), _| cid != corp_id);
    for (&issuer_id, &lp) in balances {
        snap.corp_lp_balances.insert((corp_id, issuer_id), lp);
    }
}

pub fn record_lp_reward_log(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    corp_id: i64,
    amount: i64,
    reason: String,
    timestamp_unix: u64,
) {
    snap.next_entity_id += 1;
    let log_id = snap.next_entity_id;
    snap.char_lp_logs
        .entry(char_id)
        .or_default()
        .push(LpRewardLogEntry {
            log_id,
            corp_id,
            amount,
            reason,
            timestamp_unix,
        });
}

fn offer_to_pyrep(o: &LPOffer) -> PyRep {
    let req_items_rep: Vec<PyRep> = o
        .req_items
        .iter()
        .map(|&(tid, qty)| {
            PyRep::keyval([
                ("typeID", PyRep::Integer(tid as i64)),
                ("quantity", PyRep::Integer(qty as i64)),
            ])
        })
        .collect();
    PyRep::keyval([
        ("offerID", PyRep::Integer(o.offer_id)),
        ("corpID", PyRep::Integer(o.corp_id)),
        ("typeID", PyRep::Integer(o.type_id as i64)),
        ("quantity", PyRep::Integer(o.quantity as i64)),
        ("lpCost", PyRep::Integer(o.lp_cost)),
        ("iskCost", PyRep::Real(o.isk_cost.to_wire_f64())),
        ("akCost", PyRep::Integer(o.ak_cost.max(0))),
        ("reqItems", PyRep::List(req_items_rep)),
    ])
}

fn check_corp_lp_role(
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    corp_id: i64,
) -> Result<(), BridgeRpcError> {
    let allowed = eve_db::corp_roles::ROLE_DIRECTOR
        | eve_db::corp_roles::ROLE_ACCOUNTANT
        | (1u64 << 42); // corpRoleBrandManager
    let mut roles = if let Some(corp) = snap.corporations.get(&corp_id) {
        if corp.ceo_id == char_id {
            return Ok(());
        }
        corp.member_roles
            .get(&char_id)
            .copied()
            .unwrap_or(session.corp_role)
    } else {
        session.corp_role
    };
    if let Some(ra) = snap.corp_role_assignments.get(&(corp_id, char_id)) {
        roles |= ra.roles;
    }
    if (roles & allowed) == 0 {
        return Err(BridgeRpcError {
            code: "InsufficientCorporationRole".into(),
            message: format!(
                "Character {char_id} lacks Director, Accountant, or Brand Manager role in corporation {corp_id}"
            ),
        });
    }
    Ok(())
}

fn take_offer(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    is_corp_take: bool,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };

    let (target_corp_id, target_offer_id, qty) = if args.len() >= 3 {
        let cid = args[0].as_i64();
        let oid = args[1].as_i64().unwrap_or(0);
        let q = args[2].as_i64().unwrap_or(1);
        (cid, oid, q)
    } else if args.len() >= 2 && args[0].as_i64().map(|id| id > 100_000).unwrap_or(false) {
        let cid = args[0].as_i64();
        let oid = args[1].as_i64().unwrap_or(0);
        let q = args.get(2).and_then(|a| a.as_i64()).unwrap_or(1);
        (cid, oid, q)
    } else {
        let oid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
        let q = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
        (None, oid, q)
    };
    if qty <= 0 {
        return Some(Err(BridgeRpcError {
            code: "InvalidQuantity".into(),
            message: format!("LP store offer quantity must be positive (got {qty})"),
        }));
    }
    let Ok(qty_i32) = i32::try_from(qty) else {
        return Some(Err(BridgeRpcError {
            code: "InvalidQuantity".into(),
            message: format!("LP store offer quantity {qty} exceeds i32::MAX"),
        }));
    };

    let all_offers = ctx.db.with_snapshot(load_lp_store_offers);
    let offer = match all_offers.into_iter().find(|o| {
        o.offer_id == target_offer_id
            && target_corp_id.map(|cid| o.corp_id == cid).unwrap_or(true)
    }) {
        Some(o) => o,
        None => {
            return Some(Err(BridgeRpcError {
                code: "OfferNotFound".into(),
                message: format!("LP store offer {target_offer_id} not found"),
            }));
        }
    };

    // Check Triglavian (500026 / 1000298) or EDENCOM (500027 / 1000299) standing gate if applicable
    let is_trig_or_edencom = matches!(offer.corp_id, 500_026 | 500_027 | 1_000_298 | 1_000_299);

    let Some(total_lp_cost) = offer.lp_cost.checked_mul(qty) else {
        return Some(Err(BridgeRpcError {
            code: "InvalidQuantity".into(),
            message: "LP store total LP cost overflows i64".into(),
        }));
    };
    let Some(total_isk_cents) = offer.isk_cost.0.checked_mul(qty) else {
        return Some(Err(BridgeRpcError {
            code: "InvalidQuantity".into(),
            message: "LP store total ISK cost overflows i64".into(),
        }));
    };
    let total_isk_cost = IskCents(total_isk_cents);
    let Some(total_items_qty) = offer.quantity.checked_mul(qty_i32) else {
        return Some(Err(BridgeRpcError {
            code: "InvalidQuantity".into(),
            message: "LP store delivered item quantity overflows i32".into(),
        }));
    };

    let Some(delivery_loc_id) = session.station_id.or(session.structure_id) else {
        return Some(Err(BridgeRpcError {
            code: "UserError".into(),
            message: "MustBeDocked".into(),
        }));
    };
    let delivery_flag = InventoryFlag::Hangar;

    let now_unix = ctx.now_unix();
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(ch) = snap.characters.get(&char_id).cloned() else {
            return Err(BridgeRpcError {
                code: "CharacterNotFound".into(),
                message: format!("Character {char_id} not found"),
            });
        };

        if is_trig_or_edencom {
            let st = ch.standings.get(&offer.corp_id).copied().unwrap_or(0.0);
            if st < 0.0 {
                return Err(BridgeRpcError {
                    code: "InsufficientStanding".into(),
                    message: format!(
                        "LP store for {} requires non-negative standing (have {st:.2})",
                        offer.corp_id
                    ),
                });
            }
        }

        // Verify required trade-in items (`req_items`) in hangar/cargo at `delivery_loc_id`
        let mut unlocked_tradein_ids_per_req: Vec<(i32, Vec<i64>)> = Vec::with_capacity(offer.req_items.len());
        for &(req_tid, req_unit_qty) in &offer.req_items {
            let Some(needed) = req_unit_qty.checked_mul(qty_i32) else {
                return Err(BridgeRpcError {
                    code: "InvalidQuantity".into(),
                    message: "LP store required trade-in item quantity overflows i32".into(),
                });
            };
            let mut matching_ids = Vec::new();
            let mut available: i32 = 0;
            for it in snap.items.values() {
                let owner_matches = if is_corp_take {
                    it.owner_id == ch.corporation_id
                } else {
                    it.owner_id == char_id
                };
                let flag_matches = if is_corp_take {
                    matches!(
                        it.flag,
                        InventoryFlag::Hangar
                            | InventoryFlag::CorpDeliveries
                            | InventoryFlag::CorpSAG1
                            | InventoryFlag::CorpSAG2
                            | InventoryFlag::CorpSAG3
                            | InventoryFlag::CorpSAG4
                            | InventoryFlag::CorpSAG5
                            | InventoryFlag::CorpSAG6
                            | InventoryFlag::CorpSAG7
                    )
                } else {
                    it.flag == delivery_flag
                };
                if owner_matches
                    && it.location_id == delivery_loc_id
                    && it.type_id == req_tid
                    && flag_matches
                    && !snap.is_item_locked_in_contract_or_job(it.item_id)
                {
                    available = available.saturating_add(it.quantity);
                    matching_ids.push(it.item_id);
                }
            }
            if available < needed {
                return Err(BridgeRpcError {
                    code: "MissingRequiredItems".into(),
                    message: format!(
                        "LP offer {} requires {needed}x item type {req_tid} in hangar (have {available})",
                        offer.offer_id
                    ),
                });
            }
            matching_ids.sort_unstable();
            unlocked_tradein_ids_per_req.push((needed, matching_ids));
        }

        let (rem_lp, lp_before) = if is_corp_take {
            let player_corp_id = ch.corporation_id;
            check_corp_lp_role(snap, session, char_id, player_corp_id)?;
            let mut corp_balances = load_corp_lp_balances(snap, player_corp_id);
            let current_lp = corp_balances.get(&offer.corp_id).copied().unwrap_or(0);
            if current_lp < total_lp_cost {
                return Err(BridgeRpcError {
                    code: "InsufficientLoyaltyPoints".into(),
                    message: format!(
                        "Corporation {player_corp_id} does not have enough LP with {} (required {total_lp_cost}, available {current_lp})",
                        offer.corp_id
                    ),
                });
            }
            
            let corp_balance = if let Some(corp) = snap.corporations.get_mut(&player_corp_id) {
                corp.ensure_wallet_divisions();
                corp.wallet_divisions[0].balance
            } else {
                IskCents(0)
            };
                
            if corp_balance < total_isk_cost {
                return Err(BridgeRpcError {
                    code: "InsufficientFunds".into(),
                    message: "Not enough ISK for corporation LP store purchase".into(),
                });
            }
            let rem = current_lp - total_lp_cost;
            corp_balances.insert(offer.corp_id, rem);
            save_corp_lp_balances(snap, player_corp_id, &corp_balances);
            (rem, current_lp)
        } else {
            let current_lp = ch.loyalty_points.get(&offer.corp_id).copied().unwrap_or(0);
            if current_lp < total_lp_cost {
                return Err(BridgeRpcError {
                    code: "InsufficientLoyaltyPoints".into(),
                    message: format!(
                        "Not enough LP with corporation {} (required {total_lp_cost}, available {current_lp})",
                        offer.corp_id
                    ),
                });
            }
            if ch.balance < total_isk_cost {
                return Err(BridgeRpcError {
                    code: "InsufficientFunds".into(),
                    message: format!(
                        "Not enough ISK for LP store purchase (required {:.2}, available {:.2})",
                        total_isk_cost.to_wire_f64(),
                        ch.balance.to_wire_f64()
                    ),
                });
            }
            if offer.ak_cost > 0 {
                let ak_cost = offer.ak_cost.saturating_mul(qty);
                crate::services::project_discovery::spend_analysis_kredits_in_snap(snap, char_id, ak_cost)?;
            }
            let rem = current_lp - total_lp_cost;
            if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
                ch_mut.loyalty_points.insert(offer.corp_id, rem);
            }
            if total_lp_cost > 0 {
                record_lp_reward_log(
                    snap,
                    char_id,
                    offer.corp_id,
                    -total_lp_cost,
                    format!(
                        "LP store purchase: offer {} ({}x type {})",
                        offer.offer_id, total_items_qty, offer.type_id
                    ),
                    now_unix,
                );
            }
            (rem, current_lp)
        };

        // Consume required items strictly from pre-verified unlocked matching items
        for (mut remaining, candidate_ids) in unlocked_tradein_ids_per_req {
            for iid in candidate_ids {
                if remaining <= 0 {
                    break;
                }
                let remove_whole = if let Some(it) = snap.items.get_mut(&iid) {
                    if it.quantity <= remaining {
                        remaining -= it.quantity;
                        true
                    } else {
                        it.quantity -= remaining;
                        it.version += 1;
                        remaining = 0;
                        false
                    }
                } else {
                    false
                };
                if remove_whole {
                    snap.items.remove(&iid);
                }
            }
        }

        // Deduct ISK
        let bal_after = if is_corp_take {
            let player_corp_id = ch.corporation_id;
            let ba = if let Some(corp_mut) = snap.corporations.get_mut(&player_corp_id) {
                corp_mut.ensure_wallet_divisions();
                let cur = corp_mut.wallet_divisions[0].balance;
                let rem = cur.checked_sub(total_isk_cost).unwrap_or(IskCents(0));
                corp_mut.wallet_divisions[0].balance = rem;
                corp_mut.master_wallet = rem;
                rem
            } else {
                IskCents(0)
            };
            
            snap.append_journal(
                player_corp_id,
                "LPStorePurchase",
                total_isk_cost,
                ba,
                format!(
                    "Corporation LP store purchase: offer {} ({}x type {}) by char {}",
                    offer.offer_id, total_items_qty, offer.type_id, char_id
                ),
            );
            ba
        } else {
            let ba = if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
                ch_mut.balance = ch_mut
                    .balance
                    .checked_sub(total_isk_cost)
                    .unwrap_or(IskCents(0));
                ch_mut.balance
            } else {
                IskCents(0)
            };

            snap.append_journal(
                char_id,
                "LPStorePurchase",
                total_isk_cost,
                ba,
                format!(
                    "LP store purchase: offer {} ({}x type {})",
                    offer.offer_id, total_items_qty, offer.type_id
                ),
            );
            ba
        };

        // Deliver item to hangar
        snap.next_entity_id += 1;
        let new_item_id = snap.next_entity_id;
        let final_owner_id = if is_corp_take { ch.corporation_id } else { char_id };
        let final_flag = if is_corp_take { eve_db::types::InventoryFlag::CorpDeliveries } else { delivery_flag };
        snap.items.insert(
            new_item_id,
            InventoryItemRecord {
                item_id: new_item_id,
                type_id: offer.type_id,
                owner_id: final_owner_id,
                location_id: delivery_loc_id,
                flag: final_flag,
                quantity: total_items_qty,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );

        Ok((new_item_id, offer.type_id, total_items_qty, rem_lp, bal_after, lp_before))
    });

    match res {
        Ok((item_id, type_id, qty_delivered, rem_lp, bal_after, lp_before)) => {
            if is_corp_take {
                let corp_id = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0));
                ctx.notify(
                    crate::router::NotificationTarget::Corporation(corp_id),
                    "OnCorpLPChange",
                    "corpid",
                    vec![
                        PyRep::String("take_offer".into()),
                        PyRep::Integer(offer.corp_id),
                    ],
                );
            } else {
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnLPChange",
                    "charid",
                    vec![
                        PyRep::String("take_offer".into()),
                        PyRep::Integer(offer.corp_id),
                        PyRep::Integer(lp_before),
                        PyRep::Integer(rem_lp),
                    ],
                );
            }
            Some(Ok(PyRep::keyval([
                ("offerID", PyRep::Integer(offer.offer_id)),
                ("corpID", PyRep::Integer(offer.corp_id)),
                ("deliveredItemID", PyRep::Integer(item_id)),
                ("deliveredTypeID", PyRep::Integer(type_id as i64)),
                ("quantity", PyRep::Integer(qty_delivered as i64)),
                ("remainingLP", PyRep::Integer(rem_lp)),
                ("balanceAfterISK", PyRep::Real(bal_after.to_wire_f64())),
            ])))
        }
        Err(e) => Some(Err(e)),
    }
}

fn get_character_lp_balances(
    ctx: &ServerContext,
    session: &SessionState,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };

    let rows = ctx.db.with_snapshot(|snap| {
        let Some(ch) = snap.characters.get(&char_id) else {
            return Vec::new();
        };
        let mut entries: Vec<(i64, i64)> =
            ch.loyalty_points.iter().map(|(&c, &lp)| (c, lp)).collect();
        entries.sort_by_key(|&(c, _)| c);
        entries
            .into_iter()
            .map(|(corp_id, lp)| {
                PyRep::keyval([
                    ("corpID", PyRep::Integer(corp_id)),
                    ("loyaltyPoints", PyRep::Integer(lp)),
                    ("lp", PyRep::Integer(lp)),
                ])
            })
            .collect()
    });

    Some(Ok(PyRep::List(rows)))
}

fn transfer_lp(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    is_from_corp: bool,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let method_name = if is_from_corp {
        "TransferLPFromMyCorpWalletToOtherCorp"
    } else {
        "TransferLPFromMyWalletToOtherCorp"
    };
    let Some(target_corp_id) = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0) else {
        return Some(Err(BridgeRpcError {
            code: "InvalidTargetCorp".into(),
            message: format!("{method_name} requires targetCorpID"),
        }));
    };
    let npc_corp_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1_000_035);
    let amount = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
    if amount <= 0 {
        return Some(Err(BridgeRpcError {
            code: "InvalidLPAmount".into(),
            message: "LP transfer amount must be positive".into(),
        }));
    }
    let now_unix = ctx.now_unix();
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(ch) = snap.characters.get(&char_id).cloned() else {
            return Err(BridgeRpcError {
                code: "CharacterNotFound".into(),
                message: format!("Character {char_id} not found"),
            });
        };
        let (sender_remaining, lp_before) = if is_from_corp {
            let sender_corp_id = ch.corporation_id;
            check_corp_lp_role(snap, session, char_id, sender_corp_id)?;
            let mut sender_bals = load_corp_lp_balances(snap, sender_corp_id);
            let cur = sender_bals.get(&npc_corp_id).copied().unwrap_or(0);
            if cur < amount {
                return Err(BridgeRpcError {
                    code: "InsufficientLoyaltyPoints".into(),
                    message: format!(
                        "Corporation {sender_corp_id} has {cur} LP with {npc_corp_id}, cannot transfer {amount}"
                    ),
                });
            }
            let rem = cur - amount;
            sender_bals.insert(npc_corp_id, rem);
            save_corp_lp_balances(snap, sender_corp_id, &sender_bals);
            (rem, cur)
        } else {
            let cur = ch.loyalty_points.get(&npc_corp_id).copied().unwrap_or(0);
            if cur < amount {
                return Err(BridgeRpcError {
                    code: "InsufficientLoyaltyPoints".into(),
                    message: format!(
                        "Character {char_id} has {cur} LP with {npc_corp_id}, cannot transfer {amount}"
                    ),
                });
            }
            let rem = cur - amount;
            if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
                ch_mut.loyalty_points.insert(npc_corp_id, rem);
            }
            (rem, cur)
        };

        let mut dest_bals = load_corp_lp_balances(snap, target_corp_id);
        let new_dest = dest_bals.get(&npc_corp_id).copied().unwrap_or(0).saturating_add(amount);
        dest_bals.insert(npc_corp_id, new_dest);
        save_corp_lp_balances(snap, target_corp_id, &dest_bals);

        record_lp_reward_log(
            snap,
            char_id,
            npc_corp_id,
            -amount,
            format!("Transferred {amount} LP to corporation {target_corp_id}"),
            now_unix,
        );
        Ok((sender_remaining, new_dest, lp_before))
    });

    match res {
        Ok((sender_rem, dest_lp, lp_before)) => {
            if is_from_corp {
                let sender_corp_id = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0));
                ctx.notify(
                    crate::router::NotificationTarget::Corporation(sender_corp_id),
                    "OnCorpLPChange",
                    "corpid",
                    vec![
                        PyRep::String("transfer".into()),
                        PyRep::Integer(npc_corp_id),
                    ],
                );
            } else {
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnLPChange",
                    "charid",
                    vec![
                        PyRep::String("transfer".into()),
                        PyRep::Integer(npc_corp_id),
                        PyRep::Integer(lp_before),
                        PyRep::Integer(sender_rem),
                    ],
                );
            }
            ctx.notify(
                crate::router::NotificationTarget::Corporation(target_corp_id),
                "OnCorpLPChange",
                "corpid",
                vec![
                    PyRep::String("transfer".into()),
                    PyRep::Integer(npc_corp_id),
                ],
            );
            Some(Ok(PyRep::keyval([
                ("targetCorpID", PyRep::Integer(target_corp_id)),
                ("npcCorpID", PyRep::Integer(npc_corp_id)),
                ("transferredLP", PyRep::Integer(amount)),
                ("senderRemainingLP", PyRep::Integer(sender_rem)),
                ("targetCorpLPBalance", PyRep::Integer(dest_lp)),
            ])))
        }
        Err(e) => Some(Err(e)),
    }
}

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("LPStoreMgr", "GetAvailableOffersFromCorp") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id_arg = args.first().and_then(|a| a.as_i64());
            let default_corp = 1_000_035; // Caldari Navy

            let all_offers = ctx.db.with_snapshot(load_lp_store_offers);
            let matching: Vec<&LPOffer> = match corp_id_arg {
                Some(cid) => all_offers.iter().filter(|o| o.corp_id == cid).collect(),
                None => all_offers.iter().filter(|o| o.corp_id == default_corp).collect(),
            };

            let list = matching.into_iter().map(offer_to_pyrep).collect::<Vec<_>>();
            Some(Ok(PyRep::List(list)))
        }

        ("LPStoreMgr", "TakeOfferForCharacter") => take_offer(ctx, session, args, false),
        ("LPStoreMgr", "TakeOfferForCorporation") => take_offer(ctx, session, args, true),

        ("LPSvc", "GetAllMyCharacterWalletLPBalances") => get_character_lp_balances(ctx, session),

        ("LPSvc", "GetAllMyCorporationWalletLPBalances") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_corp_id = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0);
            let res = ctx.db.with_snapshot(|snap| {
                let char_corp_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .or(session.corporation_id)
                    .unwrap_or(1_000_009);
                if let Some(req_corp_id) = target_corp_id {
                    if req_corp_id != char_corp_id && session.corporation_id != Some(req_corp_id) {
                        return Err(BridgeRpcError::user_error("CrpAccessDenied"));
                    }
                }
                let corp_id = target_corp_id.unwrap_or(char_corp_id);
                let allowed_view_roles = eve_db::corp_roles::ROLE_DIRECTOR
                    | eve_db::corp_roles::ROLE_ACCOUNTANT
                    | eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT
                    | (1u64 << 42); // corpRoleBrandManager
                let is_ceo = snap
                    .corporations
                    .get(&corp_id)
                    .map(|c| c.ceo_id == char_id)
                    .unwrap_or(false);
                if !is_ceo {
                    let mut roles = snap
                        .corporations
                        .get(&corp_id)
                        .and_then(|c| c.member_roles.get(&char_id).copied())
                        .unwrap_or(session.corp_role);
                    if let Some(ra) = snap.corp_role_assignments.get(&(corp_id, char_id)) {
                        roles |= ra.roles;
                    }
                    if (roles & allowed_view_roles) == 0 {
                        return Err(BridgeRpcError::user_error("CrpAccessDenied"));
                    }
                }
                let balances = load_corp_lp_balances(snap, corp_id);
                let mut entries: Vec<(i64, i64)> = balances.into_iter().collect();
                entries.sort_by_key(|&(c, _)| c);
                Ok(entries
                    .into_iter()
                    .map(|(npc_corp_id, lp)| {
                        PyRep::keyval([
                            ("corpID", PyRep::Integer(npc_corp_id)),
                            ("ownerCorpID", PyRep::Integer(corp_id)),
                            ("loyaltyPoints", PyRep::Integer(lp)),
                            ("lp", PyRep::Integer(lp)),
                        ])
                    })
                    .collect::<Vec<_>>())
            });
            Some(res.map(PyRep::List))
        }

        ("LPSvc", "TransferLPFromMyWalletToOtherCorp") => transfer_lp(ctx, session, args, false),
        ("LPSvc", "TransferLPFromMyCorpWalletToOtherCorp") => transfer_lp(ctx, session, args, true),

        ("LPStoreMgr", "ExchangeConcordLP") | ("LPSvc", "ExchangeConcordLP") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(target_corp_id) = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidTargetCorp".into(),
                    message: "ExchangeConcordLP requires a valid target NPC corpID".into(),
                }));
            };
            if target_corp_id == CONCORD_CORP_ID
                || matches!(target_corp_id, 1_000_179..=1_000_182 | 1_000_139 | 1_000_159)
            {
                return Some(Err(BridgeRpcError {
                    code: "InvalidTargetCorp".into(),
                    message: "Cannot exchange CONCORD LP into CONCORD, Factional Warfare militia, Food Relief, or The Sanctuary LP".into(),
                }));
            }
            let Some(npc_corp) = ctx.sde.npc_corporations.get(&target_corp_id) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidTargetCorp".into(),
                    message: format!("Target corporation {target_corp_id} is not a valid NPC corporation"),
                }));
            };
            let Some(faction_id) = npc_corp.faction_id else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidTargetCorp".into(),
                    message: format!("Target NPC corporation {target_corp_id} has no eligible faction"),
                }));
            };
            // DERIVED: Published CONCORD LP conversion rates (`Loyalty_Points.md:78-81`):
            // - 1 : 0.8 for Empire corporations (Amarr, Caldari, Gallente, Minmatar, Ammatar, Khanid)
            // - 1 : 0.4 for non-pirate, non-empire corporations (Syndicate, ORE, Thukker, Sisters of EVE excl. Food Relief/Sanctuary, SoCT, non-CONCORD Assembly)
            // - Pirate factions (including Mordu's Legion 500_018) and FW militia do not accept CONCORD LP
            let conversion_rate = match faction_id {
                500_001..=500_004 | 500_007 | 500_008 => 0.8_f64,
                500_006 | 500_009 | 500_014 | 500_015 | 500_016 | 500_017 => 0.4_f64,
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidTargetCorp".into(),
                        message: format!(
                            "NPC corporation {target_corp_id} (faction {faction_id}) does not accept CONCORD LP conversion"
                        ),
                    }));
                }
            };
            let concord_amount = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            if concord_amount <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidLPAmount".into(),
                    message: "CONCORD LP exchange amount must be positive".into(),
                }));
            }
            let converted_lp = ((concord_amount as f64) * conversion_rate).floor() as i64;
            if converted_lp <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidLPAmount".into(),
                    message: "CONCORD LP amount too small to yield positive target LP".into(),
                }));
            }
            let session_station_id = session.station_id;
            let session_structure_id = session.structure_id;
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let docked_station_id = session_station_id.or(ch.station_id);
                if docked_station_id.is_none() || session_structure_id.is_some() {
                    return Err(BridgeRpcError {
                        code: "MustBeDockedInStation".into(),
                        message: "Must be docked in an NPC station to exchange CONCORD LP".into(),
                    });
                }
                let existing_target_lp = ch.loyalty_points.get(&target_corp_id).copied().unwrap_or(0);
                if existing_target_lp <= 0 {
                    return Err(BridgeRpcError {
                        code: "NoExistingTargetCorpLP".into(),
                        message: format!(
                            "Character {char_id} must already hold LP with corporation {target_corp_id} to exchange CONCORD LP"
                        ),
                    });
                }
                let cur_concord = ch.loyalty_points.get(&CONCORD_CORP_ID).copied().unwrap_or(0);
                if cur_concord < concord_amount {
                    return Err(BridgeRpcError {
                        code: "InsufficientLoyaltyPoints".into(),
                        message: format!(
                            "Insufficient CONCORD LP (required {concord_amount}, available {cur_concord})"
                        ),
                    });
                }
                let rem_concord = cur_concord - concord_amount;
                ch.loyalty_points.insert(CONCORD_CORP_ID, rem_concord);
                let new_target = ch
                    .loyalty_points
                    .get(&target_corp_id)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(converted_lp);
                ch.loyalty_points.insert(target_corp_id, new_target);
                record_lp_reward_log(
                    snap,
                    char_id,
                    target_corp_id,
                    converted_lp,
                    format!("CONCORD LP exchange ({concord_amount} CONCORD LP -> {converted_lp} LP)"),
                    now_unix,
                );
                Ok((rem_concord, new_target))
            });

            match res {
                Ok((rem_concord, new_target)) => Some(Ok(PyRep::keyval([
                    ("targetCorpID", PyRep::Integer(target_corp_id)),
                    ("concordLPSpent", PyRep::Integer(concord_amount)),
                    ("conversionRate", PyRep::Real(conversion_rate)),
                    ("convertedLP", PyRep::Integer(converted_lp)),
                    ("remainingConcordLP", PyRep::Integer(rem_concord)),
                    ("newTargetCorpLP", PyRep::Integer(new_target)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("rewardMgr", "GetRewardLPLogs") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let logs = ctx.db.with_snapshot(|snap| {
                let entries: Vec<LpRewardLogEntry> = snap
                    .char_lp_logs
                    .get(&char_id)
                    .cloned()
                    .unwrap_or_default();
                entries
                    .into_iter()
                    .map(|e| {
                        PyRep::keyval([
                            ("logID", PyRep::Integer(e.log_id)),
                            ("corpID", PyRep::Integer(e.corp_id)),
                            ("amount", PyRep::Integer(e.amount)),
                            ("loyaltyPoints", PyRep::Integer(e.amount)),
                            ("reason", PyRep::String(e.reason)),
                            ("timestamp", PyRep::Integer(e.timestamp_unix as i64)),
                        ])
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(logs)))
        }

        // parity: rpc:rewardMgr.GetDelayedRewardsByGroupIDs
        ("rewardMgr", "GetDelayedRewardsByGroupIDs") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let group_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
                    items.iter().filter_map(|v| v.as_i64()).collect()
                }
                Some(PyRep::Integer(id)) => vec![*id],
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArguments".into(),
                        message: "rewardMgr.GetDelayedRewardsByGroupIDs requires a list of rewardGroupIDs".into(),
                    }));
                }
            };
            let lp_logs: Vec<LpRewardLogEntry> = ctx.db.with_snapshot(|snap| {
                snap.char_lp_logs.get(&char_id).cloned().unwrap_or_default()
            });
            let inc_logs = crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
                reg.incursion_lp_logs.get(&char_id).cloned().unwrap_or_default()
            });
            let mut by_group = Vec::with_capacity(group_ids.len());
            for gid in group_ids {
                let mut rewards = Vec::new();
                for e in &lp_logs {
                    if e.corp_id == gid || e.log_id == gid {
                        rewards.push(PyRep::keyval([
                            ("rewardGroupID", PyRep::Integer(gid)),
                            ("logID", PyRep::Integer(e.log_id)),
                            ("corpID", PyRep::Integer(e.corp_id)),
                            ("rewardQuantity", PyRep::Integer(e.amount)),
                            ("amount", PyRep::Integer(e.amount)),
                            ("loyaltyPoints", PyRep::Integer(e.amount)),
                            ("reason", PyRep::String(e.reason.clone())),
                            ("timestamp", PyRep::Integer(e.timestamp_unix as i64)),
                        ]));
                    }
                }
                for inc in &inc_logs {
                    if inc.constellation_id as i64 == gid
                        || inc.solar_system_id as i64 == gid
                        || inc.log_id == gid
                    {
                        rewards.push(PyRep::keyval([
                            ("rewardGroupID", PyRep::Integer(gid)),
                            ("logID", PyRep::Integer(inc.log_id)),
                            ("constellationID", PyRep::Integer(inc.constellation_id as i64)),
                            ("solarSystemID", PyRep::Integer(inc.solar_system_id as i64)),
                            ("rewardQuantity", PyRep::Integer(inc.concord_lp_earned)),
                            ("amount", PyRep::Integer(inc.isk_earned)),
                            ("loyaltyPoints", PyRep::Integer(inc.concord_lp_earned)),
                            ("reason", PyRep::String(format!("Incursion {} ({})", inc.tier, inc.site_id))),
                            ("timestamp", PyRep::Integer(inc.timestamp_filetime)),
                        ]));
                    }
                }
                by_group.push((PyRep::Integer(gid), PyRep::List(rewards)));
            }
            Some(Ok(PyRep::Dict(by_group)))
        }

        _ => None,
    }
}

