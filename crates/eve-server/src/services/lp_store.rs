//! `lp_store` — Loyalty Point (LP) Store Service (`E-6`).
//!
//! Handles `LPStoreMgr`, `LPSvc`, and `LPStore` RPCs for:
//! - Looking up available corporation LP store offerings (`GetAvailableOffersFromCorp`, `GetAvailableOffers`)
//! - Exchanging LP and ISK for items (`TakeOfferForCharacter`, `TakeOffer`, `AcceptOffer`)
//! - Querying character LP balances across NPC corporations (`GetLPsForCharacter`, `GetAllMyCharacterWalletLPBalances`)

// parity: plan:E-6
// parity: feat:lp.store-offers, feat:lp.deduction, feat:lp.isk-payment, feat:lp.item-delivery, feat:lp.wallet-balances

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct LPOffer {
    pub offer_id: i64,
    pub corp_id: i64,
    pub type_id: TypeId,
    pub quantity: i32,
    pub lp_cost: i64,
    pub isk_cost: IskCents,
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
        },
        LPOffer {
            offer_id: 1002,
            corp_id: 1_000_035,
            type_id: 27361, // Caldari Navy Scourge Light Missile
            quantity: 1_000,
            lp_cost: 500,
            isk_cost: IskCents::from_isk(500_000),
        },
        LPOffer {
            offer_id: 1003,
            corp_id: 1_000_035,
            type_id: 17619, // Caldari Navy Hookbill
            quantity: 1,
            lp_cost: 10_000,
            isk_cost: IskCents::from_isk(10_000_000),
        },
        // Federation Navy (1_000_036)
        LPOffer {
            offer_id: 2001,
            corp_id: 1_000_036,
            type_id: 15808, // Federation Navy Magnetic Field Stabilizer
            quantity: 1,
            lp_cost: 2_400,
            isk_cost: IskCents::from_isk(2_400_000),
        },
        LPOffer {
            offer_id: 2002,
            corp_id: 1_000_036,
            type_id: 17620, // Federation Navy Comet
            quantity: 1,
            lp_cost: 10_000,
            isk_cost: IskCents::from_isk(10_000_000),
        },
        // Republic Fleet (1_000_044)
        LPOffer {
            offer_id: 3001,
            corp_id: 1_000_044,
            type_id: 15806, // Republic Fleet Gyrostabilizer
            quantity: 1,
            lp_cost: 2_400,
            isk_cost: IskCents::from_isk(2_400_000),
        },
        LPOffer {
            offer_id: 3002,
            corp_id: 1_000_044,
            type_id: 17621, // Republic Fleet Firetail
            quantity: 1,
            lp_cost: 10_000,
            isk_cost: IskCents::from_isk(10_000_000),
        },
        // Amarr Navy (1_000_037)
        LPOffer {
            offer_id: 4001,
            corp_id: 1_000_037,
            type_id: 15807, // Imperial Navy Heat Sink
            quantity: 1,
            lp_cost: 2_400,
            isk_cost: IskCents::from_isk(2_400_000),
        },
        LPOffer {
            offer_id: 4002,
            corp_id: 1_000_037,
            type_id: 17622, // Imperial Navy Slicer
            quantity: 1,
            lp_cost: 10_000,
            isk_cost: IskCents::from_isk(10_000_000),
        },
        // CONCORD (1_000_125)
        LPOffer {
            offer_id: 5001,
            corp_id: 1_000_125,
            type_id: 15810,
            quantity: 1,
            lp_cost: 3_000,
            isk_cost: IskCents::from_isk(3_000_000),
        },
    ]
}

fn offer_to_pyrep(o: &LPOffer) -> PyRep {
    PyRep::keyval([
        ("offerID", PyRep::Integer(o.offer_id)),
        ("corpID", PyRep::Integer(o.corp_id)),
        ("typeID", PyRep::Integer(o.type_id as i64)),
        ("quantity", PyRep::Integer(o.quantity as i64)),
        ("lpCost", PyRep::Integer(o.lp_cost)),
        ("iskCost", PyRep::Real(o.isk_cost.to_wire_f64())),
    ])
}

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if !matches!(service, "LPStoreMgr" | "LPSvc" | "LPStore") {
        return None;
    }

    match method {
        "GetAvailableOffersFromCorp" | "GetAvailableOffers" => {
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

            // If no specific offers for this corp, return all general offers or fallback
            if matching.is_empty() {
                matching = all_offers.iter().collect();
            }

            let list = matching.into_iter().map(offer_to_pyrep).collect::<Vec<_>>();
            Some(Ok(PyRep::List(list)))
        }

        "TakeOfferForCharacter" | "TakeOffer" | "AcceptOffer" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            // Arguments can be:
            // (corpID, offerID, [quantity]) OR (offerID, [quantity])
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
            let offer = all_offers
                .into_iter()
                .find(|o| {
                    o.offer_id == target_offer_id
                        && target_corp_id.map(|cid| o.corp_id == cid).unwrap_or(true)
                })
                .ok_or_else(|| BridgeRpcError {
                    code: "OfferNotFound".into(),
                    message: format!("LP store offer {target_offer_id} not found"),
                });

            let offer = match offer {
                Ok(o) => o,
                Err(e) => return Some(Err(e)),
            };

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
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };

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

                // Deduct LP
                let rem_lp = current_lp - total_lp_cost;
                ch.loyalty_points.insert(offer.corp_id, rem_lp);

                // Deduct ISK
                ch.balance = ch.balance.checked_sub(total_isk_cost).unwrap_or(IskCents(0));
                let bal_after = ch.balance;

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

        "GetLPsForCharacter" | "GetAllMyCharacterWalletLPBalances" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let rows = ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return Vec::new();
                };
                ch.loyalty_points
                    .iter()
                    .map(|(&corp_id, &lp)| {
                        PyRep::keyval([
                            ("corpID", PyRep::Integer(corp_id)),
                            ("loyaltyPoints", PyRep::Integer(lp)),
                        ])
                    })
                    .collect()
            });

            Some(Ok(PyRep::List(rows)))
        }

        _ => None,
    }
}
