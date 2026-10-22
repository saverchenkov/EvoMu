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
// parity: feat:lp.lp-balance, feat:lp.lp-store-offers, feat:lp.lp-store-buy
// parity: feat:lp.lp-corp-wallet, feat:lp.concord-lp, feat:lp.lp-reward-logs, feat:wallet.evermarks

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

const CONCORD_CORP_ID: i64 = 1_000_125;
pub const PARAGON_CORP_ID: i64 = 1_000_419;

#[derive(Debug, Clone)]
pub struct LPOffer {
    pub offer_id: i64,
    pub corp_id: i64,
    pub type_id: TypeId,
    pub quantity: i32,
    pub lp_cost: i64,
    pub isk_cost: IskCents,
    pub req_items: Vec<(TypeId, i32)>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LpRewardLogEntry {
    pub log_id: i64,
    pub corp_id: i64,
    pub amount: i64,
    pub reason: String,
    pub timestamp_unix: u64,
}

fn corp_lp_note_id(corp_id: i64) -> i64 {
    -(4_000_000_000_000_i64.saturating_add(corp_id.abs()))
}

pub fn load_corp_lp_balances(snap: &eve_db::WorldStateSnapshot, corp_id: i64) -> HashMap<i64, i64> {
    snap.owner_notes
        .get(&corp_lp_note_id(corp_id))
        .and_then(|n| serde_json::from_str::<HashMap<i64, i64>>(&n.note).ok())
        .unwrap_or_default()
}

pub fn save_corp_lp_balances(
    snap: &mut eve_db::WorldStateSnapshot,
    corp_id: i64,
    balances: &HashMap<i64, i64>,
) {
    let nid = corp_lp_note_id(corp_id);
    if let Ok(js) = serde_json::to_string(balances) {
        snap.owner_notes.insert(
            nid,
            eve_db::OwnerNoteRecord {
                note_id: nid,
                character_id: -4,
                item_id: corp_id,
                label: format!("corp_lp:{corp_id}"),
                note: js,
                timestamp_unix: 0,
            },
        );
    }
}

fn lp_reward_log_note_id(char_id: i64) -> i64 {
    -(4_100_000_000_000_i64.saturating_add(char_id.abs()))
}

pub fn record_lp_reward_log(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    corp_id: i64,
    amount: i64,
    reason: String,
    timestamp_unix: u64,
) {
    let nid = lp_reward_log_note_id(char_id);
    let mut logs: Vec<LpRewardLogEntry> = snap
        .owner_notes
        .get(&nid)
        .and_then(|n| serde_json::from_str(&n.note).ok())
        .unwrap_or_default();
    snap.next_entity_id += 1;
    logs.push(LpRewardLogEntry {
        log_id: snap.next_entity_id,
        corp_id,
        amount,
        reason,
        timestamp_unix,
    });
    if let Ok(js) = serde_json::to_string(&logs) {
        snap.owner_notes.insert(
            nid,
            eve_db::OwnerNoteRecord {
                note_id: nid,
                character_id: -4,
                item_id: char_id,
                label: format!("lp_logs:{char_id}"),
                note: js,
                timestamp_unix,
            },
        );
    }
}

/// DERIVED (OF-36): Live EVE V24 client SDE (`fsdbinary` / `sqlite`) does not ship NPC corporation LP store offer tables;
/// those reside exclusively on CCP's server database (`lpStore`). All `type_id`s and `corp_id`s below are verified SDE IDs,
/// and `isk_cost` is stored and multiplied as exact integer `IskCents`.
fn get_catalog_offers() -> Vec<LPOffer> {
    vec![
        // Caldari Navy (1_000_035)
        LPOffer {
            offer_id: 1001,
            corp_id: 1_000_035,
            type_id: 15810, // Caldari Navy Ballistic Control System
            quantity: 1,
            lp_cost: 2_400,
            isk_cost: IskCents::from_isk(2_400_000),
            req_items: Vec::new(),
        },
        LPOffer {
            offer_id: 1002,
            corp_id: 1_000_035,
            type_id: 27361, // Caldari Navy Scourge Light Missile
            quantity: 1_000,
            lp_cost: 500,
            isk_cost: IskCents::from_isk(500_000),
            req_items: Vec::new(),
        },
        LPOffer {
            offer_id: 1003,
            corp_id: 1_000_035,
            type_id: 17619, // Caldari Navy Hookbill
            quantity: 1,
            lp_cost: 10_000,
            isk_cost: IskCents::from_isk(10_000_000),
            req_items: Vec::new(),
        },
        LPOffer {
            offer_id: 1004,
            corp_id: 1_000_035,
            type_id: 15810, // Caldari Navy Ballistic Control System (trade-in offer with reqItems)
            quantity: 1,
            lp_cost: 1_500,
            isk_cost: IskCents::from_isk(1_500_000),
            req_items: vec![(34, 100)], // Requires 100x Tritanium (type 34) trade-in
        },
        // Federation Navy (1_000_036)
        LPOffer {
            offer_id: 2001,
            corp_id: 1_000_036,
            type_id: 15808, // Federation Navy Magnetic Field Stabilizer
            quantity: 1,
            lp_cost: 2_400,
            isk_cost: IskCents::from_isk(2_400_000),
            req_items: Vec::new(),
        },
        LPOffer {
            offer_id: 2002,
            corp_id: 1_000_036,
            type_id: 17620, // Federation Navy Comet
            quantity: 1,
            lp_cost: 10_000,
            isk_cost: IskCents::from_isk(10_000_000),
            req_items: Vec::new(),
        },
        // Republic Fleet (1_000_044)
        LPOffer {
            offer_id: 3001,
            corp_id: 1_000_044,
            type_id: 15806, // Republic Fleet Gyrostabilizer
            quantity: 1,
            lp_cost: 2_400,
            isk_cost: IskCents::from_isk(2_400_000),
            req_items: Vec::new(),
        },
        LPOffer {
            offer_id: 3002,
            corp_id: 1_000_044,
            type_id: 17621, // Republic Fleet Firetail
            quantity: 1,
            lp_cost: 10_000,
            isk_cost: IskCents::from_isk(10_000_000),
            req_items: Vec::new(),
        },
        // Amarr Navy (1_000_037)
        LPOffer {
            offer_id: 4001,
            corp_id: 1_000_037,
            type_id: 15807, // Imperial Navy Heat Sink
            quantity: 1,
            lp_cost: 2_400,
            isk_cost: IskCents::from_isk(2_400_000),
            req_items: Vec::new(),
        },
        LPOffer {
            offer_id: 4002,
            corp_id: 1_000_037,
            type_id: 17622, // Imperial Navy Slicer
            quantity: 1,
            lp_cost: 10_000,
            isk_cost: IskCents::from_isk(10_000_000),
            req_items: Vec::new(),
        },
        // CONCORD (1_000_125)
        LPOffer {
            offer_id: 5001,
            corp_id: CONCORD_CORP_ID,
            type_id: 15810,
            quantity: 1,
            lp_cost: 3_000,
            isk_cost: IskCents::from_isk(3_000_000),
            req_items: Vec::new(),
        },
        // DERIVED: Paragon (1_000_419, corpHeraldry) EverMarks store offers (`feat:wallet.evermarks`)
        LPOffer {
            offer_id: 6001,
            corp_id: PARAGON_CORP_ID,
            type_id: 72964, // Rifter Corporation Emblem
            quantity: 1,
            lp_cost: 750,
            isk_cost: IskCents(0),
            req_items: Vec::new(),
        },
        LPOffer {
            offer_id: 6002,
            corp_id: PARAGON_CORP_ID,
            type_id: 73377, // Heron Corporation Emblem
            quantity: 1,
            lp_cost: 750,
            isk_cost: IskCents(0),
            req_items: Vec::new(),
        },
        LPOffer {
            offer_id: 6003,
            corp_id: PARAGON_CORP_ID,
            type_id: 73394, // Rifter Alliance Emblem
            quantity: 1,
            lp_cost: 1_250,
            isk_cost: IskCents(0),
            req_items: Vec::new(),
        },
        LPOffer {
            offer_id: 6004,
            corp_id: PARAGON_CORP_ID,
            type_id: 81348, // Alignment Sequencer
            quantity: 10,
            lp_cost: 1_000,
            isk_cost: IskCents(0),
            req_items: Vec::new(),
        },
        // 24th Imperial Crusade (1_000_179, Amarr militia, `feat:fw.fw-lp`)
        LPOffer {
            offer_id: 7001,
            corp_id: 1_000_179,
            type_id: 17622, // Imperial Navy Slicer
            quantity: 1,
            lp_cost: 10_000,
            isk_cost: IskCents::from_isk(10_000_000),
            req_items: Vec::new(),
        },
        // State Protectorate (1_000_180, Caldari militia, `feat:fw.fw-lp`)
        LPOffer {
            offer_id: 7002,
            corp_id: 1_000_180,
            type_id: 17619, // Caldari Navy Hookbill
            quantity: 1,
            lp_cost: 10_000,
            isk_cost: IskCents::from_isk(10_000_000),
            req_items: Vec::new(),
        },
        // Federal Defence Union (1_000_181, Gallente militia, `feat:fw.fw-lp`)
        LPOffer {
            offer_id: 7003,
            corp_id: 1_000_181,
            type_id: 17620, // Federation Navy Comet
            quantity: 1,
            lp_cost: 10_000,
            isk_cost: IskCents::from_isk(10_000_000),
            req_items: Vec::new(),
        },
        // Tribal Liberation Force (1_000_182, Minmatar militia, `feat:fw.fw-lp`)
        LPOffer {
            offer_id: 7004,
            corp_id: 1_000_182,
            type_id: 17621, // Republic Fleet Firetail
            quantity: 1,
            lp_cost: 10_000,
            isk_cost: IskCents::from_isk(10_000_000),
            req_items: Vec::new(),
        },
    ]
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
        ("akCost", PyRep::Integer(0)),
        ("reqItems", PyRep::List(req_items_rep)),
    ])
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

    let (target_corp_id, target_offer_id, qty) = if args.len() >= 2
        && args[0].as_i64().map(|id| id > 100_000).unwrap_or(false)
    {
        let cid = args[0].as_i64().unwrap_or(1_000_035);
        let oid = args[1].as_i64().unwrap_or(0);
        let q = args.get(2).and_then(|a| a.as_i64()).unwrap_or(1).max(1);
        (Some(cid), oid, q)
    } else {
        let oid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
        let q = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1).max(1);
        (None, oid, q)
    };

    let all_offers = get_catalog_offers();
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

    let total_lp_cost = offer.lp_cost.saturating_mul(qty);
    let total_isk_cost = IskCents(offer.isk_cost.0.saturating_mul(qty));
    let total_items_qty = offer.quantity.saturating_mul(qty as i32);

    let Some((delivery_loc_id, delivery_flag)) = session
        .station_id
        .or(session.structure_id)
        .map(|loc| (loc, InventoryFlag::Hangar))
        .or_else(|| session.ship_id.map(|s| (s, InventoryFlag::Cargo)))
    else {
        return Some(Err(BridgeRpcError {
            code: "NoDeliveryLocation".into(),
            message: "Accepting LP offer requires being docked at a station or in an active ship".into(),
        }));
    };

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
        for &(req_tid, req_unit_qty) in &offer.req_items {
            let needed = req_unit_qty.saturating_mul(qty as i32);
            let available: i32 = snap
                .items
                .values()
                .filter(|it| {
                    it.owner_id == char_id
                        && it.location_id == delivery_loc_id
                        && it.type_id == req_tid
                        && it.flag != InventoryFlag::Locked
                        && !snap.is_item_locked_in_contract_or_job(it.item_id)
                })
                .map(|it| it.quantity)
                .sum();
            if available < needed {
                return Err(BridgeRpcError {
                    code: "MissingRequiredItems".into(),
                    message: format!(
                        "LP offer {} requires {needed}x item type {req_tid} in hangar (have {available})",
                        offer.offer_id
                    ),
                });
            }
        }

        let rem_lp = if is_corp_take {
            let player_corp_id = ch.corporation_id;
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
            if ch.balance < total_isk_cost {
                return Err(BridgeRpcError {
                    code: "InsufficientFunds".into(),
                    message: "Not enough ISK for corporation LP store purchase".into(),
                });
            }
            let rem = current_lp - total_lp_cost;
            corp_balances.insert(offer.corp_id, rem);
            save_corp_lp_balances(snap, player_corp_id, &corp_balances);
            rem
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
            let rem = current_lp - total_lp_cost;
            if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
                ch_mut.loyalty_points.insert(offer.corp_id, rem);
            }
            rem
        };

        // Consume required items
        for &(req_tid, req_unit_qty) in &offer.req_items {
            let mut remaining = req_unit_qty.saturating_mul(qty as i32);
            let mut to_remove = Vec::new();
            for it in snap.items.values_mut() {
                if remaining <= 0 {
                    break;
                }
                if it.owner_id == char_id
                    && it.location_id == delivery_loc_id
                    && it.type_id == req_tid
                    && it.flag != InventoryFlag::Locked
                {
                    if it.quantity <= remaining {
                        remaining -= it.quantity;
                        to_remove.push(it.item_id);
                    } else {
                        it.quantity -= remaining;
                        it.version += 1;
                        remaining = 0;
                    }
                }
            }
            for iid in to_remove {
                snap.items.remove(&iid);
            }
        }

        // Deduct ISK
        let bal_after = if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
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
            bal_after,
            format!(
                "LP store purchase: offer {} ({}x type {})",
                offer.offer_id, total_items_qty, offer.type_id
            ),
        );

        // Deliver item to hangar
        snap.next_entity_id += 1;
        let new_item_id = snap.next_entity_id;
        snap.items.insert(
            new_item_id,
            InventoryItemRecord {
                item_id: new_item_id,
                type_id: offer.type_id,
                owner_id: char_id,
                location_id: delivery_loc_id,
                flag: delivery_flag,
                quantity: total_items_qty,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );

        Ok((new_item_id, offer.type_id, total_items_qty, rem_lp, bal_after))
    });

    match res {
        Ok((item_id, type_id, qty_delivered, rem_lp, bal_after)) => {
            if is_corp_take {
                let corp_id = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0));
                ctx.notify(
                    crate::router::NotificationTarget::Corporation(corp_id),
                    "OnCorpLPChange",
                    "corpid",
                    vec![PyRep::Integer(offer.corp_id), PyRep::Integer(rem_lp)],
                );
            } else {
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnLPChange",
                    "charid",
                    vec![PyRep::Integer(offer.corp_id), PyRep::Integer(rem_lp)],
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
        let sender_remaining = if is_from_corp {
            let sender_corp_id = ch.corporation_id;
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
            rem
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
            rem
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
        Ok((sender_remaining, new_dest))
    });

    match res {
        Ok((sender_rem, dest_lp)) => {
            if is_from_corp {
                let sender_corp_id = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0));
                ctx.notify(
                    crate::router::NotificationTarget::Corporation(sender_corp_id),
                    "OnCorpLPChange",
                    "corpid",
                    vec![PyRep::Integer(npc_corp_id), PyRep::Integer(sender_rem)],
                );
            } else {
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnLPChange",
                    "charid",
                    vec![PyRep::Integer(npc_corp_id), PyRep::Integer(sender_rem)],
                );
            }
            ctx.notify(
                crate::router::NotificationTarget::Corporation(target_corp_id),
                "OnCorpLPChange",
                "corpid",
                vec![PyRep::Integer(npc_corp_id), PyRep::Integer(dest_lp)],
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
            let target_corp = corp_id_arg.unwrap_or(default_corp);

            let all_offers = get_catalog_offers();
            let mut matching: Vec<&LPOffer> = all_offers
                .iter()
                .filter(|o| o.corp_id == target_corp)
                .collect();

            if matching.is_empty() {
                matching = all_offers.iter().collect();
            }

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
            let target_corp_id = args.first().and_then(|a| a.as_i64());
            let rows = ctx.db.with_snapshot(|snap| {
                let corp_id = target_corp_id.unwrap_or_else(|| {
                    snap.characters
                        .get(&char_id)
                        .map(|c| c.corporation_id)
                        .unwrap_or(1_000_009)
                });
                let balances = load_corp_lp_balances(snap, corp_id);
                let mut entries: Vec<(i64, i64)> = balances.into_iter().collect();
                entries.sort_by_key(|&(c, _)| c);
                entries
                    .into_iter()
                    .map(|(npc_corp_id, lp)| {
                        PyRep::keyval([
                            ("corpID", PyRep::Integer(npc_corp_id)),
                            ("ownerCorpID", PyRep::Integer(corp_id)),
                            ("loyaltyPoints", PyRep::Integer(lp)),
                            ("lp", PyRep::Integer(lp)),
                        ])
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(rows)))
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
            if target_corp_id == CONCORD_CORP_ID {
                return Some(Err(BridgeRpcError {
                    code: "InvalidTargetCorp".into(),
                    message: "Cannot exchange CONCORD LP into CONCORD LP".into(),
                }));
            }
            let concord_amount = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            if concord_amount <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidLPAmount".into(),
                    message: "CONCORD LP exchange amount must be positive".into(),
                }));
            }
            // Published CONCORD LP conversion rate: 0.8 for Empire Navy / standard NPC corps
            let conversion_rate = 0.8_f64;
            let converted_lp = ((concord_amount as f64) * conversion_rate).floor() as i64;
            if converted_lp <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidLPAmount".into(),
                    message: "CONCORD LP amount too small to yield positive target LP".into(),
                }));
            }
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
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
                let nid = lp_reward_log_note_id(char_id);
                let entries: Vec<LpRewardLogEntry> = snap
                    .owner_notes
                    .get(&nid)
                    .and_then(|n| serde_json::from_str(&n.note).ok())
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

        ("rewardMgr", "GetDelayedRewardsByGroupIDs") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let group_ids = match args.first() {
                Some(PyRep::List(list)) | Some(PyRep::Tuple(list)) => {
                    list.iter().filter_map(|v| v.as_i64()).collect::<Vec<i64>>()
                }
                Some(PyRep::Integer(gid)) => vec![*gid],
                _ => Vec::new(),
            };
            let result = ctx.db.with_snapshot(|snap| {
                let ext = crate::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap);
                let char_rewards = ext.delayed_rewards.get(&char_id);
                let mut dict = Vec::new();
                for gid in group_ids {
                    let matching: Vec<PyRep> = char_rewards
                        .map(|rewards| {
                            rewards
                                .iter()
                                .filter(|r| r.reward_group_id == gid)
                                .map(|r| {
                                    PyRep::keyval([
                                        ("rewardGroupID", PyRep::Integer(r.reward_group_id)),
                                        ("rewardQuantity", PyRep::Integer(r.quantity)),
                                        ("rewardTypeID", PyRep::Integer(r.reward_type_id)),
                                    ])
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    dict.push((PyRep::Integer(gid), PyRep::List(matching)));
                }
                dict
            });
            Some(Ok(PyRep::Dict(result)))
        }

        _ => None,
    }
}

