//! `market_contracts` — Domain 7: Regional Market Order Book, Matching Engine, Wallet/PLEX, & Contracts (`§3.7`).

#![forbid(unsafe_code)]

use crate::formulas::{compute_broker_fee_rate, compute_sales_tax_rate};
use crate::router::ServerContext;
use eve_db::{
    ContractRecord, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
    WalletJournalEntry,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("account", "GetCashBalance") | ("wallet", "GetBalance") | ("walletSvc", "GetBalance") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let bal = ctx
                .db
                .get_character(char_id)
                .map(|c| c.balance.to_wire_f64())
                .unwrap_or(0.0);
            Some(Ok(PyRep::Real(bal)))
        }

        ("account", "GetJournal") | ("wallet", "GetJournal") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.wallet_journal
                    .iter()
                    .filter(|j| j.character_id == char_id)
                    .map(|j| {
                        vec![
                            PyRep::Integer(j.entry_id),
                            PyRep::String(j.entry_type.clone()),
                            PyRep::Real(j.amount.to_wire_f64()),
                            PyRep::Real(j.balance_after.to_wire_f64()),
                            PyRep::String(j.reason.clone()),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["entryID", "entryType", "amount", "balance", "reason"],
                rows,
            )))
        }

        ("marketProxy", "GetOrders") | ("market", "GetOrders") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let type_id = args.first().and_then(|a| a.as_i64()).unwrap_or(34) as TypeId;
            let rows = ctx.db.with_snapshot(|snap| {
                snap.market_orders
                    .values()
                    .filter(|o| o.type_id == type_id && o.vol_remaining > 0)
                    .map(|o| {
                        vec![
                            PyRep::Integer(o.order_id),
                            PyRep::Integer(o.type_id as i64),
                            PyRep::Bool(o.is_bid),
                            PyRep::Real(o.price.to_wire_f64()),
                            PyRep::Integer(o.vol_remaining as i64),
                            PyRep::Integer(o.station_id),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec![
                    "orderID",
                    "typeID",
                    "bid",
                    "price",
                    "volRemaining",
                    "stationID",
                ],
                rows,
            )))
        }

        ("marketProxy", "PlaceCharOrder") | ("market", "PlaceOrder") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let station_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.station_id)
                .unwrap_or(60_003_760);
            let type_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(34) as TypeId;
            let price_isk = args.get(2).and_then(|a| a.as_f64()).unwrap_or(10.0);
            let quantity = args.get(3).and_then(|a| a.as_i64()).unwrap_or(100) as i32;
            let is_bid = args.get(4).and_then(|a| a.as_bool()).unwrap_or(true);

            if !price_isk.is_finite() || price_isk <= 0.0 || quantity <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMarketOrder".into(),
                    message: "Market order requires finite price > 0 and quantity > 0".into(),
                }));
            }

            let region_id = session.region_id.unwrap_or(10_000_002);
            let station_in_region = ctx.sde.solar_systems.values().any(|sys| {
                sys.region_id == region_id
                    && sys.stations.iter().any(|st| st.station_id == station_id)
            });
            if !station_in_region {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMarketStation".into(),
                    message: format!("Station {station_id} is not in region {region_id}"),
                }));
            }

            let broker_rate = compute_broker_fee_rate(4, 2.0, 2.0);
            let tax_rate = compute_sales_tax_rate(4);
            let order_price = IskCents::from_f64_isk(price_isk);
            let gross_isk = price_isk * (quantity as f64);
            let broker_fee = IskCents::from_f64_isk(gross_isk * broker_rate);
            let sales_tax = if is_bid {
                IskCents(0)
            } else {
                IskCents::from_f64_isk(gross_isk * tax_rate)
            };

            let res = ctx.db.with_mut_snapshot(|snap| {
                // 1. If sell order (`!is_bid`), verify and deduct `quantity` of `type_id` from seller's hangar at `station_id`
                if !is_bid {
                    let sell_item_id = snap
                        .items
                        .values()
                        .find(|i| {
                            i.owner_id == char_id
                                && i.location_id == station_id
                                && i.flag == InventoryFlag::Hangar
                                && i.type_id == type_id
                                && i.quantity >= quantity
                        })
                        .map(|i| i.item_id);
                    let Some(iid) = sell_item_id else {
                        return Err(BridgeRpcError {
                            code: "InsufficientInventoryForSellOrder".into(),
                            message: format!(
                                "Seller does not have {quantity}x type {type_id} in station {station_id} hangar"
                            ),
                        });
                    };
                    if let Some(item) = snap.items.get_mut(&iid) {
                        if item.quantity > quantity {
                            item.quantity -= quantity;
                            item.version += 1;
                        } else {
                            snap.items.remove(&iid);
                        }
                    }
                }

                // 2. Collect matching opposite orders sorted by best price-time priority
                let mut candidate_ids: Vec<(i64, i64)> = snap
                    .market_orders
                    .values()
                    .filter(|o| {
                        o.type_id == type_id
                            && o.station_id == station_id
                            && o.is_bid != is_bid
                            && o.vol_remaining > 0
                            && ((is_bid && o.price.0 <= order_price.0)
                                || (!is_bid && o.price.0 >= order_price.0))
                    })
                    .map(|o| (o.order_id, o.price.0))
                    .collect();

                if is_bid {
                    // Best (lowest) ask first, then lowest order_id (FIFO)
                    candidate_ids.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
                } else {
                    // Best (highest) bid first, then lowest order_id (FIFO)
                    candidate_ids.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                }

                // Pre-check caller balance for worst-case debit
                let max_order_cost = IskCents(order_price.0 * (quantity as i64));
                let caller_balance = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.balance.0)
                    .unwrap_or(0);
                if is_bid && caller_balance < max_order_cost.0 + broker_fee.0 {
                    return Err(BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: "Not enough ISK for buy order and broker fee".into(),
                    });
                }
                if !is_bid && caller_balance < broker_fee.0 {
                    return Err(BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: "Not enough ISK to pay sell order broker fee".into(),
                    });
                }

                // Debit broker fee upfront
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.balance = IskCents(ch.balance.0 - broker_fee.0);
                }

                let mut remaining_qty = quantity;
                let mut first_matched_oid = None;

                for (oid, _) in candidate_ids {
                    if remaining_qty <= 0 {
                        break;
                    }
                    let Some(resting) = snap.market_orders.get(&oid).cloned() else {
                        continue;
                    };
                    let fill_qty = remaining_qty.min(resting.vol_remaining);
                    if fill_qty <= 0 {
                        continue;
                    }
                    let fill_gross = IskCents(resting.price.0 * (fill_qty as i64));
                    let fill_tax =
                        IskCents(((fill_gross.0 as f64) * tax_rate).round() as i64);

                    if is_bid {
                        // Caller is buying from resting sell order (`resting`)
                        if let Some(buyer) = snap.characters.get_mut(&char_id) {
                            buyer.balance = IskCents(buyer.balance.0 - fill_gross.0);
                            let bal_after = buyer.balance;
                            snap.next_entity_id += 1;
                            let jid = snap.next_entity_id;
                            snap.wallet_journal.push(WalletJournalEntry {
                                entry_id: jid,
                                character_id: char_id,
                                entry_type: "MarketTransaction".into(),
                                amount: fill_gross,
                                balance_after: bal_after,
                                reason: format!("Bought {fill_qty}x type {type_id}"),
                            });
                        }
                        if !resting.is_npc {
                            if let Some(seller) = snap.characters.get_mut(&resting.owner_id) {
                                let seller_net = (fill_gross.0 - fill_tax.0).max(0);
                                seller.balance = IskCents(seller.balance.0 + seller_net);
                            }
                        }
                        // Deliver items to buyer
                        snap.next_entity_id += 1;
                        let new_item_id = snap.next_entity_id;
                        snap.items.insert(
                            new_item_id,
                            InventoryItemRecord {
                                item_id: new_item_id,
                                type_id,
                                owner_id: char_id,
                                location_id: station_id,
                                flag: InventoryFlag::Hangar,
                                quantity: fill_qty,
                                singleton: false,
                                version: 1,
                                dynamic_attributes: HashMap::new(),
                                source_mutaplasmid_id: None,
                                insured_payout_isk: 0.0,
                            },
                        );
                    } else {
                        // Caller is selling into resting buy order (`resting` already escrowed funds)
                        let seller_net = (fill_gross.0 - fill_tax.0).max(0);
                        if let Some(seller) = snap.characters.get_mut(&char_id) {
                            seller.balance = IskCents(seller.balance.0 + seller_net);
                            let bal_after = seller.balance;
                            snap.next_entity_id += 1;
                            let jid = snap.next_entity_id;
                            snap.wallet_journal.push(WalletJournalEntry {
                                entry_id: jid,
                                character_id: char_id,
                                entry_type: "MarketSell".into(),
                                amount: IskCents(seller_net),
                                balance_after: bal_after,
                                reason: format!("Sold {fill_qty}x type {type_id}"),
                            });
                        }
                        if !resting.is_npc {
                            snap.next_entity_id += 1;
                            let new_item_id = snap.next_entity_id;
                            snap.items.insert(
                                new_item_id,
                                InventoryItemRecord {
                                    item_id: new_item_id,
                                    type_id,
                                    owner_id: resting.owner_id,
                                    location_id: station_id,
                                    flag: InventoryFlag::Hangar,
                                    quantity: fill_qty,
                                    singleton: false,
                                    version: 1,
                                    dynamic_attributes: HashMap::new(),
                                    source_mutaplasmid_id: None,
                                    insured_payout_isk: 0.0,
                                },
                            );
                        }
                    }

                    if let Some(o_mut) = snap.market_orders.get_mut(&oid) {
                        o_mut.vol_remaining -= fill_qty;
                        if o_mut.is_bid {
                            o_mut.escrow = IskCents((o_mut.escrow.0 - fill_gross.0).max(0));
                        }
                    }
                    remaining_qty -= fill_qty;
                    if first_matched_oid.is_none() {
                        first_matched_oid = Some(oid);
                    }
                }

                if remaining_qty == 0 {
                    return Ok((first_matched_oid.unwrap_or(0), true));
                }

                // Place unfilled remainder as a resting order on the book
                let escrow = if is_bid {
                    let esc = IskCents(order_price.0 * (remaining_qty as i64));
                    if let Some(buyer) = snap.characters.get_mut(&char_id) {
                        buyer.balance = IskCents(buyer.balance.0 - esc.0);
                    }
                    esc
                } else {
                    IskCents(0)
                };

                snap.next_entity_id += 1;
                let new_oid = snap.next_entity_id;
                snap.market_orders.insert(
                    new_oid,
                    MarketOrderRecord {
                        order_id: new_oid,
                        owner_id: char_id,
                        station_id,
                        region_id,
                        type_id,
                        is_bid,
                        price: order_price,
                        vol_entered: quantity,
                        vol_remaining: remaining_qty,
                        min_volume: 1,
                        is_npc: false,
                        escrow,
                    },
                );
                Ok((new_oid, false))
            });

            match res {
                Ok((order_id, filled_immediately)) => Some(Ok(PyRep::keyval([
                    ("orderID", PyRep::Integer(order_id)),
                    ("filledImmediately", PyRep::Bool(filled_immediately)),
                    ("brokerFeeISK", PyRep::Real(broker_fee.to_wire_f64())),
                    ("salesTaxISK", PyRep::Real(sales_tax.to_wire_f64())),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        // Contracts (`contractMgr` / `contractProxy`): ItemExchange, Auction, Courier (`E2E-15`)
        ("contractMgr", "CreateContract") | ("contractProxy", "CreateContract") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let contract_type = args
                .first()
                .and_then(|a| a.as_str())
                .unwrap_or("Courier");
            let start_station_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(60_003_760);
            let end_station_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(60_003_766);
            let reward_isk = args.get(3).and_then(|a| a.as_f64()).unwrap_or(500_000.0);
            let collateral_isk = args.get(4).and_then(|a| a.as_f64()).unwrap_or(2_000_000.0);

            if !reward_isk.is_finite()
                || reward_isk < 0.0
                || !collateral_isk.is_finite()
                || collateral_isk < 0.0
            {
                return Some(Err(BridgeRpcError {
                    code: "InvalidContractParameters".into(),
                    message: "Contract reward and collateral must be non-negative finite numbers"
                        .into(),
                }));
            }

            let res = ctx.db.with_mut_snapshot(|snap| {
                let reward = IskCents::from_f64_isk(reward_isk);
                let collateral = IskCents::from_f64_isk(collateral_isk);
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                if ch.balance.0 < reward.0 {
                    return Err(BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: "Not enough ISK to escrow contract reward".into(),
                    });
                }
                ch.balance = IskCents(ch.balance.0 - reward.0);

                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.contracts.insert(
                    id,
                    ContractRecord {
                        contract_id: id,
                        issuer_id: char_id,
                        acceptor_id: None,
                        contract_type: contract_type.to_string(),
                        status: "Outstanding".into(),
                        start_station_id,
                        end_station_id,
                        price: IskCents(0),
                        reward,
                        collateral,
                        item_ids: Vec::new(),
                    },
                );
                Ok(id)
            });

            match res {
                Ok(cid) => Some(Ok(PyRep::keyval([
                    ("contractID", PyRep::Integer(cid)),
                    ("status", PyRep::String("Outstanding".into())),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("contractMgr", "AcceptContract") | ("contractProxy", "AcceptContract") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let cid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let res = ctx.db.with_mut_snapshot(|snap| {
                let Some(c) = snap.contracts.get(&cid).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ContractNotFound".into(),
                        message: format!("Contract {cid} does not exist"),
                    });
                };
                if c.status != "Outstanding" {
                    return Err(BridgeRpcError {
                        code: "InvalidContractState".into(),
                        message: format!("Contract {cid} is in state '{}', not Outstanding", c.status),
                    });
                }
                let required_isk = IskCents(c.collateral.0 + c.price.0);
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                if ch.balance.0 < required_isk.0 {
                    return Err(BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: "Not enough ISK to cover contract collateral and price".into(),
                    });
                }
                ch.balance = IskCents(ch.balance.0 - required_isk.0);
                if c.price.0 > 0 {
                    if let Some(issuer) = snap.characters.get_mut(&c.issuer_id) {
                        issuer.balance = IskCents(issuer.balance.0 + c.price.0);
                    }
                }
                if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                    c_mut.acceptor_id = Some(char_id);
                    c_mut.status = "InProgress".into();
                }
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("contractID", PyRep::Integer(cid)),
                    ("status", PyRep::String("InProgress".into())),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("contractMgr", "CompleteContract") | ("contractProxy", "CompleteContract") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let cid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let res = ctx.db.with_mut_snapshot(|snap| {
                let Some(c) = snap.contracts.get(&cid).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ContractNotFound".into(),
                        message: format!("Contract {cid} does not exist"),
                    });
                };
                if c.status != "InProgress" {
                    return Err(BridgeRpcError {
                        code: "InvalidContractState".into(),
                        message: format!("Contract {cid} is in state '{}', not InProgress", c.status),
                    });
                }
                if c.acceptor_id != Some(char_id) {
                    return Err(BridgeRpcError {
                        code: "ContractOwnershipViolation".into(),
                        message: format!("Contract {cid} is not accepted by character {char_id}"),
                    });
                }
                if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                    c_mut.status = "Completed".into();
                }
                let total_return = IskCents(c.collateral.0 + c.reward.0);
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.balance = IskCents(ch.balance.0 + total_return.0);
                }
                Ok(total_return.to_wire_f64())
            });
            match res {
                Ok(payout) => Some(Ok(PyRep::keyval([
                    ("contractID", PyRep::Integer(cid)),
                    ("status", PyRep::String("Completed".into())),
                    ("totalPayoutISK", PyRep::Real(payout)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("contractMgr", "GetInitVals")
        | ("contractProxy", "GetMyExpiredContractList")
        | ("trademgr", "InitiateTrade")
        | ("raffleSvc", "GetActiveTickets")
        | ("storeManager", "GetCatalog")
        | ("multiBuy", "BuyMultiple") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::List(vec![])))
        }

        _ => None,
    }
}
