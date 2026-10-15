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

            let broker_rate = compute_broker_fee_rate(4, 2.0, 2.0);
            let tax_rate = compute_sales_tax_rate(4);
            let gross_isk = price_isk * (quantity as f64);
            let broker_fee = IskCents::from_f64_isk(gross_isk * broker_rate);
            let sales_tax = if is_bid {
                IskCents(0)
            } else {
                IskCents::from_f64_isk(gross_isk * tax_rate)
            };

            let res = ctx.db.with_mut_snapshot(|snap| {
                // Check for immediate matching opposite order
                let mut matched_order_id = None;
                for o in snap.market_orders.values() {
                    if o.type_id == type_id
                        && o.station_id == station_id
                        && o.is_bid != is_bid
                        && o.vol_remaining >= quantity
                        && ((is_bid && o.price.to_wire_f64() <= price_isk)
                            || (!is_bid && o.price.to_wire_f64() >= price_isk))
                    {
                        matched_order_id = Some(o.order_id);
                        break;
                    }
                }

                let trade_cost = IskCents::from_f64_isk(gross_isk);
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    if is_bid {
                        let total_debit = trade_cost.0 + broker_fee.0;
                        if ch.balance.0 < total_debit {
                            return Err(BridgeRpcError {
                                code: "InsufficientFunds".into(),
                                message: "Not enough ISK for buy order and broker fee".into(),
                            });
                        }
                        ch.balance = IskCents(ch.balance.0 - total_debit);
                    } else {
                        let net_credit = (trade_cost.0 - broker_fee.0 - sales_tax.0).max(0);
                        ch.balance = IskCents(ch.balance.0 + net_credit);
                    }
                    let bal_after = ch.balance;
                    snap.next_entity_id += 1;
                    let jid = snap.next_entity_id;
                    snap.wallet_journal.push(WalletJournalEntry {
                        entry_id: jid,
                        character_id: char_id,
                        entry_type: if is_bid {
                            "MarketEscrowOrBuy".into()
                        } else {
                            "MarketSell".into()
                        },
                        amount: trade_cost,
                        balance_after: bal_after,
                        reason: format!("Market order type {type_id} x {quantity}"),
                    });
                }

                if let Some(oid) = matched_order_id {
                    if let Some(o) = snap.market_orders.get_mut(&oid) {
                        o.vol_remaining -= quantity;
                    }
                    if is_bid {
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
                                quantity,
                                singleton: false,
                                version: 1,
                                dynamic_attributes: HashMap::new(),
                                source_mutaplasmid_id: None,
                                insured_payout_isk: 0.0,
                            },
                        );
                    }
                    return Ok((oid, true));
                }

                snap.next_entity_id += 1;
                let new_oid = snap.next_entity_id;
                snap.market_orders.insert(
                    new_oid,
                    MarketOrderRecord {
                        order_id: new_oid,
                        owner_id: char_id,
                        station_id,
                        region_id: session.region_id.unwrap_or(10_000_002),
                        type_id,
                        is_bid,
                        price: IskCents::from_f64_isk(price_isk),
                        vol_entered: quantity,
                        vol_remaining: quantity,
                        min_volume: 1,
                        is_npc: false,
                        escrow: if is_bid { trade_cost } else { IskCents(0) },
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

            let cid = ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                let reward = IskCents::from_f64_isk(reward_isk);
                let collateral = IskCents::from_f64_isk(collateral_isk);
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.balance = IskCents((ch.balance.0 - reward.0).max(0));
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
                        item_ids: Vec::new(),
                    },
                );
                id
            });

            Some(Ok(PyRep::keyval([
                ("contractID", PyRep::Integer(cid)),
                ("status", PyRep::String("Outstanding".into())),
            ])))
        }

        ("contractMgr", "AcceptContract") | ("contractProxy", "AcceptContract") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let cid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            ctx.db.with_mut_snapshot(|snap| {
                let collateral = snap
                    .contracts
                    .get(&cid)
                    .map(|c| c.collateral)
                    .unwrap_or(IskCents(0));
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.balance = IskCents((ch.balance.0 - collateral.0).max(0));
                }
                if let Some(c) = snap.contracts.get_mut(&cid) {
                    c.acceptor_id = Some(char_id);
                    c.status = "InProgress".into();
                }
            });
            Some(Ok(PyRep::keyval([
                ("contractID", PyRep::Integer(cid)),
                ("status", PyRep::String("InProgress".into())),
            ])))
        }

        ("contractMgr", "CompleteContract") | ("contractProxy", "CompleteContract") => {
            let cid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let payout = ctx.db.with_mut_snapshot(|snap| {
                let Some(c) = snap.contracts.get_mut(&cid) else {
                    return 0.0;
                };
                c.status = "Completed".into();
                let acceptor = c.acceptor_id.unwrap_or(c.issuer_id);
                let total_return = IskCents(c.collateral.0 + c.reward.0);
                if let Some(ch) = snap.characters.get_mut(&acceptor) {
                    ch.balance = IskCents(ch.balance.0 + total_return.0);
                }
                total_return.to_wire_f64()
            });
            Some(Ok(PyRep::keyval([
                ("contractID", PyRep::Integer(cid)),
                ("status", PyRep::String("Completed".into())),
                ("totalPayoutISK", PyRep::Real(payout)),
            ])))
        }

        ("trademgr", _)
        | ("TradeService", _)
        | ("raffleSvc", _)
        | ("raffleMgr", _)
        | ("vgsabandoned", _)
        | ("storeManager", _)
        | ("plexVault", _)
        | ("multiBuy", _) => Some(Ok(PyRep::Bool(true))),

        _ => None,
    }
}
