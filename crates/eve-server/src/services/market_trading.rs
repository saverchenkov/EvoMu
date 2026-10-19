//! `market_trading` — Regional Market Order Book, Matching Engine, Limit Orders, Escrow, Fees, & Price History (`E-1`, `E-2`).
//!
//! Handles `marketProxy` RPCs for:
//! - Limit orders (`PlaceCharOrder` with `vol_entered`, `vol_remaining`, `escrow`)
//! - Immediate order matching against opposite resting orders (Price-Time Priority)
//! - Escrow ISK deduction for buy orders and escrow refund on cancellation
//! - Broker fee calculation (`broker_fee_rate = 0.03 - 0.003 * broker_relations`, plus standing modifiers)
//! - Sales tax calculation (`compute_sales_tax_rate` with Accounting skill discount)
//! - Order cancellation with escrow and item refund (`CancelCharOrder`)
//! - Regional order book queries (`GetOrders`, `GetStationAsks`, `GetRegionBest`, `GetCharOrders`)
//! - Price history queries (`GetOldPriceHistory`, `GetNewPriceHistory`)
//! - Atomic multi-item buy order execution (`BuyMultipleItems`)

// parity: plan:E-1, plan:E-2
// parity: feat:market.order-matching, feat:market.limit-orders, feat:market.escrow
// parity: feat:market.broker-fee, feat:market.sales-tax, feat:market.order-cancellation
// parity: feat:market.order-book, feat:market.price-history, feat:market.buy-multiple

#![forbid(unsafe_code)]

use crate::formulas::{compute_broker_fee_rate, compute_sales_tax_rate};
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

/// Computes exact integer sales tax in ISK cents (`i128` parts-per-million rounded to nearest cent)
/// from the seller's own Accounting (`16622`) skill level (`R21-P1-4`).
pub fn compute_sales_tax_cents(fill_gross: IskCents, seller_acct_lvl: u8) -> IskCents {
    let tax_rate = compute_sales_tax_rate(seller_acct_lvl);
    let tax_ppm = (tax_rate * 1_000_000.0).round() as i128;
    IskCents((((fill_gross.0 as i128) * tax_ppm + 500_000) / 1_000_000) as i64)
}

fn order_to_packed_cells(o: &MarketOrderRecord) -> Vec<PyRep> {
    vec![
        PyRep::Real(o.price.to_wire_f64()),
        PyRep::Real(o.vol_remaining as f64),
        PyRep::Integer(o.type_id as i64),
        PyRep::Integer(o.order_id),
        PyRep::Integer(o.vol_entered as i64),
        PyRep::Integer(o.min_volume as i64),
        PyRep::Bool(o.is_bid),
        PyRep::Integer(o.station_id),
        PyRep::Integer(o.region_id as i64),
    ]
}

fn best_asks_to_summary_rows<'a>(
    orders: impl Iterator<Item = &'a MarketOrderRecord>,
) -> Vec<Vec<PyRep>> {
    let mut best_by_type: std::collections::BTreeMap<TypeId, &MarketOrderRecord> =
        std::collections::BTreeMap::new();
    for o in orders {
        match best_by_type.get(&o.type_id) {
            Some(prev) if o.price.0 >= prev.price.0 => {}
            _ => {
                best_by_type.insert(o.type_id, o);
            }
        }
    }
    best_by_type
        .into_values()
        .map(|o| {
            vec![
                PyRep::Integer(o.type_id as i64),
                PyRep::Real(o.price.to_wire_f64()),
                PyRep::Real(o.vol_remaining as f64),
                PyRep::Integer(o.station_id),
            ]
        })
        .collect()
}

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "marketProxy" {
        return None;
    }

    match method {
        "GetOrders" => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(region_id) = session.region_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoRegion".into(),
                    message: "Session has no active region".into(),
                }));
            };
            let Some(raw_type_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMarketOrder".into(),
                    message: "GetOrders requires typeID".into(),
                }));
            };
            let type_id = raw_type_id as TypeId;
            let (sell_rows, buy_rows) = ctx.db.with_snapshot(|snap| {
                let mut sells = Vec::new();
                let mut buys = Vec::new();
                let mut matching: Vec<&MarketOrderRecord> = snap
                    .market_orders
                    .values()
                    .filter(|o| {
                        o.type_id == type_id && o.vol_remaining > 0 && o.region_id == region_id
                    })
                    .collect();
                matching.sort_by_key(|o| o.order_id);
                for o in matching {
                    let cells = order_to_packed_cells(o);
                    if o.is_bid {
                        buys.push(cells);
                    } else {
                        sells.push(cells);
                    }
                }
                (sells, buys)
            });
            let hdr = DBRowDescriptor::market_order_header();
            Some(Ok(PyRep::Tuple(vec![
                hdr.build_crowset(sell_rows),
                hdr.build_crowset(buy_rows),
            ])))
        }

        "GetStationAsks" => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let Some(station_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NotDockedAtStation".into(),
                    message: "GetStationAsks requires being docked at a station or structure"
                        .into(),
                }));
            };
            let Some(region_id) = session.region_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoRegion".into(),
                    message: "Session has no active region".into(),
                }));
            };
            let rows = ctx.db.with_snapshot(|snap| {
                best_asks_to_summary_rows(snap.market_orders.values().filter(|o| {
                    !o.is_bid
                        && o.vol_remaining > 0
                        && o.region_id == region_id
                        && o.station_id == station_id
                }))
            });
            Some(Ok(DBRowDescriptor::market_summary_header()
                .build_cindexed_rowset("typeID", rows)))
        }

        "GetRegionBest" => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let Some(region_id) = session.region_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoRegion".into(),
                    message: "Session has no active region".into(),
                }));
            };
            let rows = ctx.db.with_snapshot(|snap| {
                best_asks_to_summary_rows(
                    snap.market_orders
                        .values()
                        .filter(|o| !o.is_bid && o.vol_remaining > 0 && o.region_id == region_id),
                )
            });
            Some(Ok(DBRowDescriptor::market_summary_header()
                .build_cindexed_rowset("typeID", rows)))
        }

        "GetCharOrders" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let filter_type_id = args
                .first()
                .and_then(|a| a.as_i64())
                .filter(|&tid| tid > 0)
                .map(|tid| tid as TypeId);
            let rows = ctx.db.with_snapshot(|snap| {
                let mut orders: Vec<&MarketOrderRecord> = snap
                    .market_orders
                    .values()
                    .filter(|o| {
                        o.owner_id == char_id
                            && !o.is_npc
                            && o.vol_remaining > 0
                            && filter_type_id.map(|tid| o.type_id == tid).unwrap_or(true)
                    })
                    .collect();
                orders.sort_by_key(|o| o.order_id);
                orders
                    .into_iter()
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

        "PlaceCharOrder" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(docked_station_id) = session.station_id else {
                return Some(Err(BridgeRpcError {
                    code: "NotDockedAtStation".into(),
                    message: "Character must be docked at an NPC station to place a market order"
                        .into(),
                }));
            };
            let Some(region_id) = session.region_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoRegion".into(),
                    message: "Session has no active region".into(),
                }));
            };
            let station_id = args
                .first()
                .and_then(|a| a.as_i64())
                .unwrap_or(docked_station_id);
            let Some(raw_type_id) = args.get(1).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMarketOrder".into(),
                    message: "PlaceCharOrder requires typeID".into(),
                }));
            };
            let type_id = raw_type_id as TypeId;
            let price_isk = args.get(2).and_then(|a| a.as_f64()).unwrap_or(0.0);
            let quantity = args.get(3).and_then(|a| a.as_i64()).unwrap_or(0);
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
            let station_info = ctx.sde.solar_systems.values().find_map(|sys| {
                if sys.region_id == region_id {
                    sys.stations.iter().find(|st| st.station_id == station_id)
                } else {
                    None
                }
            });
            let Some(station_def) = station_info else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMarketStation".into(),
                    message: format!("Station {station_id} is not in region {region_id}"),
                }));
            };
            if station_id != docked_station_id {
                return Some(Err(BridgeRpcError {
                    code: "StationMismatch".into(),
                    message: format!(
                        "Cannot place market order at station {station_id} while docked at {docked_station_id}"
                    ),
                }));
            }
            let owner_corp_id = station_def.owner_corporation_id;
            let owner_faction_id = station_def.owner_faction_id;

            let (broker_lvl, acct_lvl, faction_standing, corp_standing) =
                ctx.db.with_snapshot(|snap| {
                    let sk = snap.skills.get(&char_id);
                    let b_lvl = sk.and_then(|m| m.get(&3446)).map(|s| s.level).unwrap_or(0);
                    let a_lvl = sk.and_then(|m| m.get(&16622)).map(|s| s.level).unwrap_or(0);
                    let ch = snap.characters.get(&char_id);
                    let f_st = ch
                        .and_then(|c| owner_faction_id.and_then(|fid| c.standings.get(&fid).copied()))
                        .unwrap_or(0.0);
                    let c_st = ch
                        .and_then(|c| c.standings.get(&owner_corp_id).copied())
                        .unwrap_or(0.0);
                    (b_lvl, a_lvl, f_st, c_st)
                });
            let broker_rate = compute_broker_fee_rate(broker_lvl, faction_standing, corp_standing);
            let order_price = IskCents::from_f64_isk(price_isk);
            let Some(max_order_cost) = order_price.checked_mul_qty(quantity) else {
                return Some(Err(BridgeRpcError {
                    code: "MarketOrderOverflow".into(),
                    message: "Order total cost overflows 64-bit ISK cents".into(),
                }));
            };
            let broker_ppm = (broker_rate * 1_000_000.0).round() as i128;
            let broker_fee = IskCents(
                (((max_order_cost.0 as i128) * broker_ppm + 500_000) / 1_000_000) as i64,
            );

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                // 1. Pre-check caller balance for worst-case debit using checked arithmetic
                let required_upfront = if is_bid {
                    max_order_cost.checked_add(broker_fee).ok_or_else(|| {
                        BridgeRpcError {
                            code: "MarketOrderOverflow".into(),
                            message: "Order cost + broker fee overflow".into(),
                        }
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
                            i.type_id == type_id
                                && i.quantity >= quantity_i32
                                && !i.singleton
                                && snap.is_item_consumable_by(i, char_id, Some(station_id), None)
                                && !snap.items.values().any(|other| {
                                    other.item_id != i.item_id
                                        && other.location_id == i.item_id
                                        && other.quantity > 0
                                })
                        })
                        .map(|i| i.item_id);
                    let Some(iid) = sell_item_id else {
                        let has_non_empty_candidate = snap.items.values().any(|i| {
                            i.type_id == type_id
                                && snap.is_item_consumable_by(i, char_id, Some(station_id), None)
                                && snap.items.values().any(|other| {
                                    other.item_id != i.item_id
                                        && other.location_id == i.item_id
                                        && other.quantity > 0
                                })
                        });
                        if has_non_empty_candidate {
                            return Err(BridgeRpcError {
                                code: "CannotSellItemWithContents".into(),
                                message: format!(
                                    "Cannot sell ship or container of type {type_id} while it contains fitted modules, rigs, or cargo"
                                ),
                            });
                        }
                        let has_assembled_candidate = snap.items.values().any(|i| {
                            i.type_id == type_id
                                && i.singleton
                                && snap.is_item_consumable_by(i, char_id, Some(station_id), None)
                        });
                        if has_assembled_candidate {
                            return Err(BridgeRpcError {
                                code: "CannotSellAssembledItem".into(),
                                message: format!(
                                    "Item of type {type_id} in station {station_id} hangar must be repackaged before selling on the market"
                                ),
                            });
                        }
                        return Err(BridgeRpcError {
                            code: "InsufficientInventoryForSellOrder".into(),
                            message: format!(
                                "Seller does not have {quantity_i32}x repackaged consumable type {type_id} in station {station_id} hangar"
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
                    let bal_after = ch.balance;
                    snap.append_journal(
                        char_id,
                        "MarketBrokerFee",
                        broker_fee,
                        bal_after,
                        format!("Broker fee for order on type {type_id}"),
                    );
                }

                let mut remaining_qty = quantity_i32;
                let mut first_matched_oid = None;
                let mut actual_sales_tax_charged = IskCents(0);

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

                    if is_bid {
                        if let Some(buyer) = snap.characters.get_mut(&char_id) {
                            buyer.balance = buyer
                                .balance
                                .checked_sub(fill_gross)
                                .unwrap_or(IskCents(0));
                            let bal_after = buyer.balance;
                            snap.append_journal(
                                char_id,
                                "MarketTransaction",
                                fill_gross,
                                bal_after,
                                format!("Bought {fill_qty}x type {type_id}"),
                            );
                        }
                        if !resting.is_npc {
                            let seller_acct_lvl = snap
                                .skills
                                .get(&resting.owner_id)
                                .and_then(|m| m.get(&16622))
                                .map(|s| s.level)
                                .unwrap_or(0);
                            let fill_tax = compute_sales_tax_cents(fill_gross, seller_acct_lvl);
                            if let Some(seller) = snap.characters.get_mut(&resting.owner_id) {
                                let seller_net = fill_gross
                                    .checked_sub(fill_tax)
                                    .unwrap_or(IskCents(0));
                                seller.balance = seller
                                    .balance
                                    .checked_add(seller_net)
                                    .unwrap_or(seller.balance);
                                let s_bal_after = seller.balance;
                                snap.append_journal(
                                    resting.owner_id,
                                    "MarketSell",
                                    seller_net,
                                    s_bal_after,
                                    format!("Sold {fill_qty}x type {type_id}"),
                                );
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
                                ..Default::default()
                            },
                        );
                    } else {
                        let fill_tax = compute_sales_tax_cents(fill_gross, acct_lvl);
                        actual_sales_tax_charged = actual_sales_tax_charged
                            .checked_add(fill_tax)
                            .unwrap_or(actual_sales_tax_charged);
                        let seller_net =
                            fill_gross.checked_sub(fill_tax).unwrap_or(IskCents(0));
                        if let Some(seller) = snap.characters.get_mut(&char_id) {
                            seller.balance = seller
                                .balance
                                .checked_add(seller_net)
                                .unwrap_or(seller.balance);
                            let bal_after = seller.balance;
                            snap.append_journal(
                                char_id,
                                "MarketSell",
                                seller_net,
                                bal_after,
                                format!("Sold {fill_qty}x type {type_id}"),
                            );
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
                                    ..Default::default()
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
                    return Ok((
                        first_matched_oid.unwrap_or(0),
                        true,
                        actual_sales_tax_charged,
                    ));
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
                Ok((new_oid, false, actual_sales_tax_charged))
            });

            match res {
                Ok((order_id, filled_immediately, actual_sales_tax_charged)) => {
                    Some(Ok(PyRep::keyval([
                        ("orderID", PyRep::Integer(order_id)),
                        ("filledImmediately", PyRep::Bool(filled_immediately)),
                        ("brokerFeeISK", PyRep::Real(broker_fee.to_wire_f64())),
                        (
                            "salesTaxISK",
                            PyRep::Real(actual_sales_tax_charged.to_wire_f64()),
                        ),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        "CancelCharOrder" | "CancelOrder" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(order_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMarketOrder".into(),
                    message: "CancelCharOrder requires an explicit orderID".into(),
                }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(order) = snap.market_orders.get(&order_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "OrderNotFound".into(),
                        message: format!("Market order {order_id} not found"),
                    });
                };
                if order.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "OrderOwnershipViolation".into(),
                        message: format!("Order {order_id} does not belong to character {char_id}"),
                    });
                }
                if order.vol_remaining <= 0 {
                    return Err(BridgeRpcError {
                        code: "OrderAlreadyFilled".into(),
                        message: format!("Order {order_id} is already filled or cancelled"),
                    });
                }
                if order.is_bid {
                    // Buy order: refund remaining escrow to character balance
                    if order.escrow.0 > 0 {
                        if let Some(buyer) = snap.characters.get_mut(&char_id) {
                            buyer.balance = buyer
                                .balance
                                .checked_add(order.escrow)
                                .unwrap_or(buyer.balance);
                            let bal_after = buyer.balance;
                            snap.append_journal(
                                char_id,
                                "MarketOrderCancelRefund",
                                order.escrow,
                                bal_after,
                                format!("Refunded escrow for cancelled buy order {order_id}"),
                            );
                        }
                    }
                } else {
                    // Sell order: return remaining items to seller's station hangar
                    let remaining_qty = order.vol_remaining;
                    let existing_item = snap
                        .items
                        .values()
                        .find(|i| {
                            i.owner_id == char_id
                                && i.location_id == order.station_id
                                && i.flag == InventoryFlag::Hangar
                                && i.type_id == order.type_id
                                && !i.singleton
                        })
                        .map(|i| i.item_id);

                    if let Some(eid) = existing_item {
                        if let Some(item) = snap.items.get_mut(&eid) {
                            item.quantity = item.quantity.saturating_add(remaining_qty);
                            item.version += 1;
                        }
                    } else {
                        snap.next_entity_id += 1;
                        let nid = snap.next_entity_id;
                        snap.items.insert(
                            nid,
                            InventoryItemRecord {
                                item_id: nid,
                                type_id: order.type_id,
                                owner_id: char_id,
                                location_id: order.station_id,
                                flag: InventoryFlag::Hangar,
                                quantity: remaining_qty,
                                singleton: false,
                                version: 1,
                                ..Default::default()
                            },
                        );
                    }
                }

                if let Some(o_mut) = snap.market_orders.get_mut(&order_id) {
                    o_mut.vol_remaining = 0;
                    o_mut.escrow = IskCents(0);
                }
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::Bool(true))),
                Err(e) => Some(Err(e)),
            }
        }

        "BuyMultipleItems" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(station_id) = session.station_id else {
                return Some(Err(BridgeRpcError {
                    code: "NotDockedAtStation".into(),
                    message: "Character must be docked at an NPC station to BuyMultipleItems".into(),
                }));
            };
            let raw_entries = match (args.first(), args.get(1)) {
                (Some(PyRep::List(entries) | PyRep::Tuple(entries)), _) => entries.as_slice(),
                (Some(PyRep::Integer(sid)), Some(PyRep::List(entries) | PyRep::Tuple(entries))) => {
                    if *sid != station_id {
                        return Some(Err(BridgeRpcError {
                            code: "StationMismatch".into(),
                            message: format!(
                                "BuyMultipleItems station {sid} does not match docked station {station_id}"
                            ),
                        }));
                    }
                    entries.as_slice()
                }
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidBuyMultipleItems".into(),
                        message: "BuyMultipleItems requires a non-empty list of (type_id, quantity[, max_price_isk]) entries".into(),
                    }));
                }
            };
            if raw_entries.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidBuyMultipleItems".into(),
                    message: "BuyMultipleItems requires a non-empty list of items to purchase"
                        .into(),
                }));
            }

            let mut parsed_requests: Vec<(TypeId, i32, Option<IskCents>)> =
                Vec::with_capacity(raw_entries.len());
            for entry in raw_entries {
                let (tid_i64, qty_i64, max_price_f64) = match entry {
                    PyRep::Tuple(els) | PyRep::List(els) => (
                        els.first().and_then(|e| e.as_i64()).unwrap_or(0),
                        els.get(1).and_then(|e| e.as_i64()).unwrap_or(0),
                        els.get(2).and_then(|e| e.as_f64()),
                    ),
                    PyRep::Dict(kv) => {
                        let find_key = |k: &str| {
                            kv.iter()
                                .find(|(ek, _)| ek.as_str() == Some(k))
                                .map(|(_, ev)| ev)
                        };
                        (
                            find_key("typeID").and_then(|e| e.as_i64()).unwrap_or(0),
                            find_key("quantity").and_then(|e| e.as_i64()).unwrap_or(0),
                            find_key("maxPrice")
                                .or_else(|| find_key("price"))
                                .and_then(|e| e.as_f64()),
                        )
                    }
                    _ => (0, 0, None),
                };
                let Ok(tid_u32) = u32::try_from(tid_i64) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidBuyMultipleItems".into(),
                        message: format!("Invalid type_id {tid_i64} in BuyMultipleItems"),
                    }));
                };
                let Ok(qty_i32) = i32::try_from(qty_i64) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidBuyMultipleItems".into(),
                        message: format!("Invalid quantity {qty_i64} in BuyMultipleItems"),
                    }));
                };
                if tid_u32 == 0
                    || qty_i32 <= 0
                    || !ctx
                        .sde
                        .types
                        .get(&tid_u32)
                        .map(|t| t.published)
                        .unwrap_or(false)
                {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidBuyMultipleItems".into(),
                        message: format!(
                            "BuyMultipleItems entry ({tid_u32}, {qty_i32}) must reference a published type and positive quantity"
                        ),
                    }));
                }
                let max_price_cents = match max_price_f64 {
                    Some(p) if !p.is_finite() || p <= 0.0 => {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidBuyMultipleItems".into(),
                            message: "BuyMultipleItems max_price must be finite and positive".into(),
                        }));
                    }
                    Some(p) => Some(IskCents::from_f64_isk(p)),
                    None => None,
                };
                parsed_requests.push((tid_u32, qty_i32, max_price_cents));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let buyer_bal = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.balance)
                    .ok_or_else(|| BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    })?;

                let mut virtual_rem: HashMap<i64, i32> = snap
                    .market_orders
                    .values()
                    .filter(|o| !o.is_bid && o.station_id == station_id && o.vol_remaining > 0)
                    .map(|o| (o.order_id, o.vol_remaining))
                    .collect();

                // (order_id, type_id, fill_qty, fill_gross, resting_owner_id, resting_is_npc)
                let mut planned_fills: Vec<(i64, TypeId, i32, IskCents, i64, bool)> = Vec::new();
                let mut total_cost = IskCents(0);
                let mut total_qty_bought: i64 = 0;

                for (req_tid, req_qty, max_price_opt) in &parsed_requests {
                    let mut candidate_orders: Vec<(i64, IskCents, i64, bool)> = snap
                        .market_orders
                        .values()
                        .filter(|o| {
                            !o.is_bid
                                && o.station_id == station_id
                                && o.type_id == *req_tid
                                && virtual_rem.get(&o.order_id).copied().unwrap_or(0) > 0
                                && max_price_opt.map(|mp| o.price.0 <= mp.0).unwrap_or(true)
                        })
                        .map(|o| (o.order_id, o.price, o.owner_id, o.is_npc))
                        .collect();
                    candidate_orders
                        .sort_by(|a, b| a.1.0.cmp(&b.1.0).then_with(|| a.0.cmp(&b.0)));

                    let mut rem = *req_qty;
                    for (oid, ask_price, seller_id, is_npc) in candidate_orders {
                        if rem <= 0 {
                            break;
                        }
                        let avail = virtual_rem.get(&oid).copied().unwrap_or(0);
                        let fill_qty = rem.min(avail);
                        if fill_qty <= 0 {
                            continue;
                        }
                        let fill_gross = ask_price
                            .checked_mul_qty(fill_qty as i64)
                            .ok_or_else(|| BridgeRpcError {
                                code: "MarketOrderOverflow".into(),
                                message: "Order fill cost overflowed ISK bounds".into(),
                            })?;
                        total_cost =
                            total_cost
                                .checked_add(fill_gross)
                                .ok_or_else(|| BridgeRpcError {
                                    code: "MarketOrderOverflow".into(),
                                    message: "Total BuyMultipleItems cost overflowed ISK bounds"
                                        .into(),
                                 })?;
                        if let Some(vr) = virtual_rem.get_mut(&oid) {
                            *vr -= fill_qty;
                        }
                        rem -= fill_qty;
                        total_qty_bought += fill_qty as i64;
                        planned_fills.push((oid, *req_tid, fill_qty, fill_gross, seller_id, is_npc));
                    }

                    if rem > 0 {
                        return Err(BridgeRpcError {
                            code: "InsufficientMarketLiquidity".into(),
                            message: format!(
                                "Station {station_id} lacks sufficient ask orders for type {req_tid} (short by {rem} units)"
                            ),
                        });
                    }
                }

                if buyer_bal.0 < total_cost.0 {
                    return Err(BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: format!(
                            "Insufficient ISK for BuyMultipleItems: need {:.2}, have {:.2}",
                            total_cost.to_wire_f64(),
                            buyer_bal.to_wire_f64()
                        ),
                    });
                }

                for (oid, fill_tid, fill_qty, fill_gross, seller_id, is_npc) in planned_fills {
                    if let Some(buyer) = snap.characters.get_mut(&char_id) {
                        buyer.balance = buyer
                            .balance
                            .checked_sub(fill_gross)
                            .unwrap_or(IskCents(0));
                        let bal_after = buyer.balance;
                        snap.append_journal(
                            char_id,
                            "MarketTransaction",
                            fill_gross,
                            bal_after,
                            format!("Bought {fill_qty}x type {fill_tid}"),
                        );
                    }
                    if !is_npc {
                        let seller_acct_lvl = snap
                            .skills
                            .get(&seller_id)
                            .and_then(|m| m.get(&16622))
                            .map(|s| s.level)
                            .unwrap_or(0);
                        let fill_tax = compute_sales_tax_cents(fill_gross, seller_acct_lvl);
                        let seller_net =
                            fill_gross.checked_sub(fill_tax).unwrap_or(IskCents(0));
                        if let Some(seller) = snap.characters.get_mut(&seller_id) {
                            seller.balance = seller
                                .balance
                                .checked_add(seller_net)
                                .unwrap_or(seller.balance);
                            let bal_after = seller.balance;
                            snap.append_journal(
                                seller_id,
                                "MarketSell",
                                seller_net,
                                bal_after,
                                format!("Sold {fill_qty}x type {fill_tid}"),
                            );
                        }
                    }
                    if let Some(o_mut) = snap.market_orders.get_mut(&oid) {
                        o_mut.vol_remaining -= fill_qty;
                    }
                    snap.next_entity_id += 1;
                    let new_item_id = snap.next_entity_id;
                    snap.items.insert(
                        new_item_id,
                        InventoryItemRecord {
                            item_id: new_item_id,
                            type_id: fill_tid,
                            owner_id: char_id,
                            location_id: station_id,
                            flag: InventoryFlag::Hangar,
                            quantity: fill_qty,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        },
                    );
                }

                Ok((parsed_requests.len() as i64, total_qty_bought, total_cost))
            });

            match res {
                Ok((line_count, total_qty, total_cost)) => Some(Ok(PyRep::keyval([
                    ("purchasedCount", PyRep::Integer(line_count)),
                    ("totalQuantity", PyRep::Integer(total_qty)),
                    ("totalCostISK", PyRep::Real(total_cost.to_wire_f64())),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        "GetOldPriceHistory" | "GetNewPriceHistory" => {
            let Some(region_id) = session.region_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoRegion".into(),
                    message: "Session has no active region".into(),
                }));
            };
            let Some(raw_type_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMarketOrder".into(),
                    message: "GetPriceHistory requires typeID".into(),
                }));
            };
            let type_id = raw_type_id as TypeId;
            let now_unix = ctx.now_unix();

            let rows = ctx.db.with_snapshot(|snap| {
                let orders: Vec<&MarketOrderRecord> = snap
                    .market_orders
                    .values()
                    .filter(|o| o.type_id == type_id && o.region_id == region_id)
                    .collect();
                let ref_price = if let Some(first) = orders.first() {
                    first.price.to_wire_f64()
                } else if let Some(t) = ctx.sde.types.get(&type_id) {
                    t.base_price
                } else {
                    100.0
                };
                let mut history_rows = Vec::new();
                for day_offset in (1..=30).rev() {
                    let day_sec = now_unix.saturating_sub(day_offset * 86_400);
                    let file_time = (day_sec as i64 + 11_644_473_600) * 10_000_000;
                    let low = ref_price * 0.95;
                    let high = ref_price * 1.05;
                    let avg = ref_price;
                    let volume = 100_i64;
                    let order_count = 5_i64;
                    history_rows.push(vec![
                        PyRep::Integer(file_time),
                        PyRep::Real(low),
                        PyRep::Real(high),
                        PyRep::Real(avg),
                        PyRep::Integer(volume),
                        PyRep::Integer(order_count),
                    ]);
                }
                history_rows
            });
            let hdr = DBRowDescriptor::market_history_header();
            Some(Ok(hdr.build_crowset(rows)))
        }

        _ => None,
    }
}
