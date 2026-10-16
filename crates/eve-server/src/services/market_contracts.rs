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
        ("account", "GetCashBalance") | ("vaultManager", "get_account_balance") => {
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

        ("account", "GetJournal") | ("account", "GetTransactions") => {
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

        ("marketProxy", "GetOrders") | ("marketProxy", "GetCharOrders") => {
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

        ("marketProxy", "PlaceCharOrder") | ("marketProxy", "BuyMultipleItems") => {
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
            let quantity = args.get(3).and_then(|a| a.as_i64()).unwrap_or(100);
            let is_bid = args.get(4).and_then(|a| a.as_bool()).unwrap_or(true);

            if !price_isk.is_finite()
                || price_isk <= 0.0
                || quantity <= 0
                || quantity > (i32::MAX as i64)
            {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMarketOrder".into(),
                    message: "Market order requires finite price > 0 and valid positive quantity"
                        .into(),
                }));
            }
            let quantity_i32 = quantity as i32;

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
            let Some(max_order_cost) = order_price.checked_mul_qty(quantity) else {
                return Some(Err(BridgeRpcError {
                    code: "MarketOrderOverflow".into(),
                    message: "Order total cost overflows 64-bit ISK cents".into(),
                }));
            };
            let gross_isk = max_order_cost.to_wire_f64();
            let broker_fee = IskCents::from_f64_isk(gross_isk * broker_rate);
            let sales_tax = if is_bid {
                IskCents(0)
            } else {
                IskCents::from_f64_isk(gross_isk * tax_rate)
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                // 1. Pre-check caller balance for worst-case debit using checked arithmetic
                let required_upfront = if is_bid {
                    max_order_cost
                        .checked_add(broker_fee)
                        .ok_or_else(|| BridgeRpcError {
                            code: "MarketOrderOverflow".into(),
                            message: "Order cost + broker fee overflow".into(),
                        })?
                } else {
                    broker_fee
                };

                let caller_balance = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.balance)
                    .unwrap_or(IskCents(0));
                if caller_balance.checked_sub(required_upfront).is_none() {
                    return Err(BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: "Not enough ISK for market order and broker fee".into(),
                    });
                }

                // 2. If sell order (`!is_bid`), verify and deduct `quantity_i32` of `type_id` from seller's hangar at `station_id`
                if !is_bid {
                    let sell_item_id = snap
                        .items
                        .values()
                        .find(|i| {
                            i.owner_id == char_id
                                && i.location_id == station_id
                                && i.flag == InventoryFlag::Hangar
                                && i.type_id == type_id
                                && i.quantity >= quantity_i32
                        })
                        .map(|i| i.item_id);
                    let Some(iid) = sell_item_id else {
                        return Err(BridgeRpcError {
                            code: "InsufficientInventoryForSellOrder".into(),
                            message: format!(
                                "Seller does not have {quantity_i32}x type {type_id} in station {station_id} hangar"
                            ),
                        });
                    };
                    if let Some(item) = snap.items.get_mut(&iid) {
                        if item.quantity > quantity_i32 {
                            item.quantity -= quantity_i32;
                            item.version += 1;
                        } else {
                            snap.items.remove(&iid);
                        }
                    }
                }

                // 3. Collect matching opposite orders sorted by best price-time priority
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
                    candidate_ids.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
                } else {
                    candidate_ids.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                }

                // Debit broker fee upfront
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.balance = ch.balance.checked_sub(broker_fee).unwrap_or(IskCents(0));
                }

                let mut remaining_qty = quantity_i32;
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
                    let Some(fill_gross) = resting.price.checked_mul_qty(fill_qty as i64) else {
                        continue;
                    };
                    let fill_tax = IskCents::from_f64_isk(fill_gross.to_wire_f64() * tax_rate);

                    if is_bid {
                        if let Some(buyer) = snap.characters.get_mut(&char_id) {
                            buyer.balance = buyer
                                .balance
                                .checked_sub(fill_gross)
                                .unwrap_or(IskCents(0));
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
                                let seller_net = fill_gross
                                    .checked_sub(fill_tax)
                                    .unwrap_or(IskCents(0));
                                seller.balance = seller
                                    .balance
                                    .checked_add(seller_net)
                                    .unwrap_or(seller.balance);
                            }
                        }
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
                        let seller_net =
                            fill_gross.checked_sub(fill_tax).unwrap_or(IskCents(0));
                        if let Some(seller) = snap.characters.get_mut(&char_id) {
                            seller.balance = seller
                                .balance
                                .checked_add(seller_net)
                                .unwrap_or(seller.balance);
                            let bal_after = seller.balance;
                            snap.next_entity_id += 1;
                            let jid = snap.next_entity_id;
                            snap.wallet_journal.push(WalletJournalEntry {
                                entry_id: jid,
                                character_id: char_id,
                                entry_type: "MarketSell".into(),
                                amount: seller_net,
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
                            o_mut.escrow =
                                o_mut.escrow.checked_sub(fill_gross).unwrap_or(IskCents(0));
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

                let escrow = if is_bid {
                    let esc = order_price
                        .checked_mul_qty(remaining_qty as i64)
                        .unwrap_or(IskCents(0));
                    if let Some(buyer) = snap.characters.get_mut(&char_id) {
                        buyer.balance = buyer.balance.checked_sub(esc).unwrap_or(IskCents(0));
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
                        vol_entered: quantity_i32,
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

        // Contracts (`contractProxy`): ItemExchange, Auction, Courier (`E2E-15`)
        ("contractProxy", "CreateContract") => {
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
            let item_id_opt = args.get(5).and_then(|a| a.as_i64());

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

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let reward = IskCents::from_f64_isk(reward_isk);
                let collateral = IskCents::from_f64_isk(collateral_isk);
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let new_bal = ch.balance.checked_sub(reward).ok_or_else(|| BridgeRpcError {
                    code: "InsufficientFunds".into(),
                    message: "Not enough ISK to escrow contract reward".into(),
                })?;
                ch.balance = new_bal;

                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                let mut escrowed_items = Vec::new();
                if let Some(iid) = item_id_opt {
                    let Some(item) = snap.items.get_mut(&iid) else {
                        return Err(BridgeRpcError {
                            code: "ContractItemNotFound".into(),
                            message: format!("Contract item {iid} not found"),
                        });
                    };
                    if item.owner_id != char_id || item.location_id != start_station_id {
                        return Err(BridgeRpcError {
                            code: "ContractItemOwnershipOrLocationInvalid".into(),
                            message: format!(
                                "Item {iid} must be owned by issuer at station {start_station_id}"
                            ),
                        });
                    }
                    item.location_id = id;
                    item.flag = InventoryFlag::Locked;
                    item.version += 1;
                    escrowed_items.push(iid);
                }

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
                        item_ids: escrowed_items,
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

        ("contractProxy", "AcceptContract") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let cid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(c) = snap.contracts.get(&cid).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ContractNotFound".into(),
                        message: format!("Contract {cid} does not exist"),
                    });
                };
                if c.status != "Outstanding" {
                    return Err(BridgeRpcError {
                        code: "InvalidContractState".into(),
                        message: format!(
                            "Contract {cid} is in state '{}', not Outstanding",
                            c.status
                        ),
                    });
                }
                let required_isk =
                    c.collateral.checked_add(c.price).ok_or_else(|| BridgeRpcError {
                        code: "ContractOverflow".into(),
                        message: "Collateral + price overflow".into(),
                    })?;
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let new_bal =
                    ch.balance
                        .checked_sub(required_isk)
                        .ok_or_else(|| BridgeRpcError {
                            code: "InsufficientFunds".into(),
                            message: "Not enough ISK to cover contract collateral and price".into(),
                        })?;
                ch.balance = new_bal;
                if c.price.0 > 0 {
                    if let Some(issuer) = snap.characters.get_mut(&c.issuer_id) {
                        issuer.balance = issuer
                            .balance
                            .checked_add(c.price)
                            .unwrap_or(issuer.balance);
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

        ("contractProxy", "CompleteContract") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let cid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let caller_station = session.station_id.or(session.structure_id);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(c) = snap.contracts.get(&cid).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ContractNotFound".into(),
                        message: format!("Contract {cid} does not exist"),
                    });
                };
                if c.status != "InProgress" {
                    return Err(BridgeRpcError {
                        code: "InvalidContractState".into(),
                        message: format!(
                            "Contract {cid} is in state '{}', not InProgress",
                            c.status
                        ),
                    });
                }
                if c.acceptor_id != Some(char_id) {
                    return Err(BridgeRpcError {
                        code: "ContractOwnershipViolation".into(),
                        message: format!("Contract {cid} is not accepted by character {char_id}"),
                    });
                }
                if c.contract_type == "Courier" && caller_station != Some(c.end_station_id) {
                    return Err(BridgeRpcError {
                        code: "CourierNotAtDestinationStation".into(),
                        message: format!(
                            "Completing Courier contract {cid} requires being docked at destination station {}",
                            c.end_station_id
                        ),
                    });
                }
                for iid in &c.item_ids {
                    if let Some(item) = snap.items.get_mut(iid) {
                        item.owner_id = c.issuer_id;
                        item.location_id = c.end_station_id;
                        item.flag = InventoryFlag::Hangar;
                        item.version += 1;
                    }
                }
                if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                    c_mut.status = "Completed".into();
                }
                let total_return = c
                    .collateral
                    .checked_add(c.reward)
                    .unwrap_or(c.collateral);
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.balance = ch.balance.checked_add(total_return).unwrap_or(ch.balance);
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

        ("contractProxy", "GetContractListForOwner")
        | ("contractProxy", "GetMyCurrentContractList") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.contracts
                    .values()
                    .filter(|c| c.issuer_id == char_id || c.acceptor_id == Some(char_id))
                    .map(|c| {
                        vec![
                            PyRep::Integer(c.contract_id),
                            PyRep::String(c.contract_type.clone()),
                            PyRep::String(c.status.clone()),
                            PyRep::Real(c.reward.to_wire_f64()),
                            PyRep::Real(c.collateral.to_wire_f64()),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["contractID", "type", "status", "reward", "collateral"],
                rows,
            )))
        }

        _ => None,
    }
}
