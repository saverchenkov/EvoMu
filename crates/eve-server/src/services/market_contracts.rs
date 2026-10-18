//! `market_contracts` — Domain 7: Regional Market Order Book, Matching Engine, Wallet/PLEX, & Contracts (`§3.7`).

#![forbid(unsafe_code)]

use crate::formulas::{compute_broker_fee_rate, compute_sales_tax_rate};
use crate::router::ServerContext;
use eve_db::{
    ContractRecord, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
    WalletJournalEntry,
};
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
    match (service, method) {
        ("account", "GetCashBalance") | ("vaultManager", "get_account_balance") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let bal = ctx.db.get_character(char_id).map(|c| c.balance.to_wire_f64()).unwrap_or(0.0);
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
            Some(Ok(PyRep::rowset(vec!["entryID", "entryType", "amount", "balance", "reason"], rows)))
        }

        ("marketProxy", "GetOrders") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(region_id) = session.region_id else {
                return Some(Err(BridgeRpcError { code: "NoRegion".into(), message: "Session has no active region".into() }));
            };
            let Some(raw_type_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidMarketOrder".into(), message: "GetOrders requires typeID".into() }));
            };
            let type_id = raw_type_id as TypeId;
            let (sell_rows, buy_rows) = ctx.db.with_snapshot(|snap| {
                let mut sells = Vec::new();
                let mut buys = Vec::new();
                let mut matching: Vec<&MarketOrderRecord> = snap
                    .market_orders
                    .values()
                    .filter(|o| o.type_id == type_id && o.vol_remaining > 0 && o.region_id == region_id)
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
            Some(Ok(PyRep::Tuple(vec![hdr.build_crowset(sell_rows), hdr.build_crowset(buy_rows)])))
        }

        ("marketProxy", "GetStationAsks") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let Some(station_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError { code: "NotDockedAtStation".into(), message: "GetStationAsks requires being docked at a station or structure".into() }));
            };
            let Some(region_id) = session.region_id else {
                return Some(Err(BridgeRpcError { code: "NoRegion".into(), message: "Session has no active region".into() }));
            };
            let rows = ctx.db.with_snapshot(|snap| {
                best_asks_to_summary_rows(snap.market_orders.values().filter(|o| !o.is_bid && o.vol_remaining > 0 && o.region_id == region_id && o.station_id == station_id))
            });
            Some(Ok(DBRowDescriptor::market_summary_header().build_cindexed_rowset("typeID", rows)))
        }

        ("marketProxy", "GetRegionBest") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let Some(region_id) = session.region_id else {
                return Some(Err(BridgeRpcError { code: "NoRegion".into(), message: "Session has no active region".into() }));
            };
            let rows = ctx.db.with_snapshot(|snap| {
                best_asks_to_summary_rows(snap.market_orders.values().filter(|o| !o.is_bid && o.vol_remaining > 0 && o.region_id == region_id))
            });
            Some(Ok(DBRowDescriptor::market_summary_header().build_cindexed_rowset("typeID", rows)))
        }

        ("marketProxy", "GetCharOrders") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let filter_type_id = args.first().and_then(|a| a.as_i64()).filter(|&tid| tid > 0).map(|tid| tid as TypeId);
            let rows = ctx.db.with_snapshot(|snap| {
                let mut orders: Vec<&MarketOrderRecord> = snap
                    .market_orders
                    .values()
                    .filter(|o| o.owner_id == char_id && !o.is_npc && o.vol_remaining > 0 && filter_type_id.map(|tid| o.type_id == tid).unwrap_or(true))
                    .collect();
                orders.sort_by_key(|o| o.order_id);
                orders
                    .into_iter()
                    .map(|o| vec![PyRep::Integer(o.order_id), PyRep::Integer(o.type_id as i64), PyRep::Bool(o.is_bid), PyRep::Real(o.price.to_wire_f64()), PyRep::Integer(o.vol_remaining as i64), PyRep::Integer(o.station_id)])
                    .collect()
            });
            Some(Ok(PyRep::rowset(vec!["orderID", "typeID", "bid", "price", "volRemaining", "stationID"], rows)))
        }

        ("marketProxy", "PlaceCharOrder") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(docked_station_id) = session.station_id else {
                return Some(Err(BridgeRpcError { code: "NotDockedAtStation".into(), message: "Character must be docked at an NPC station to place a market order".into() }));
            };
            let Some(region_id) = session.region_id else {
                return Some(Err(BridgeRpcError { code: "NoRegion".into(), message: "Session has no active region".into() }));
            };
            let station_id = args.first().and_then(|a| a.as_i64()).unwrap_or(docked_station_id);
            let Some(raw_type_id) = args.get(1).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidMarketOrder".into(), message: "PlaceCharOrder requires typeID".into() }));
            };
            let type_id = raw_type_id as TypeId;
            let price_isk = args.get(2).and_then(|a| a.as_f64()).unwrap_or(0.0);
            let quantity = args.get(3).and_then(|a| a.as_i64()).unwrap_or(0);
            let is_bid = args.get(4).and_then(|a| a.as_bool()).unwrap_or(true);

            if !price_isk.is_finite() || price_isk <= 0.0 || quantity <= 0 || quantity > (i32::MAX as i64) {
                return Some(Err(BridgeRpcError { code: "InvalidMarketOrder".into(), message: "Market order requires finite price > 0 and valid positive quantity".into() }));
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
                return Some(Err(BridgeRpcError { code: "InvalidMarketStation".into(), message: format!("Station {station_id} is not in region {region_id}") }));
            };
            if station_id != docked_station_id {
                return Some(Err(BridgeRpcError { code: "StationMismatch".into(), message: format!("Cannot place market order at station {station_id} while docked at {docked_station_id}") }));
            }
            let owner_corp_id = station_def.owner_corporation_id;
            let owner_faction_id = station_def.owner_faction_id;

            let (broker_lvl, acct_lvl, faction_standing, corp_standing) = ctx.db.with_snapshot(|snap| {
                let sk = snap.skills.get(&char_id);
                let b_lvl = sk.and_then(|m| m.get(&3446)).map(|s| s.level).unwrap_or(0);
                let a_lvl = sk.and_then(|m| m.get(&16622)).map(|s| s.level).unwrap_or(0);
                let ch = snap.characters.get(&char_id);
                let f_st = ch.and_then(|c| c.standings.get(&owner_faction_id).copied()).unwrap_or(0.0);
                let c_st = ch.and_then(|c| c.standings.get(&owner_corp_id).copied()).unwrap_or(0.0);
                (b_lvl, a_lvl, f_st, c_st)
            });
            let broker_rate = compute_broker_fee_rate(broker_lvl, faction_standing, corp_standing);
            let order_price = IskCents::from_f64_isk(price_isk);
            let Some(max_order_cost) = order_price.checked_mul_qty(quantity) else {
                return Some(Err(BridgeRpcError { code: "MarketOrderOverflow".into(), message: "Order total cost overflows 64-bit ISK cents".into() }));
            };
            let broker_ppm = (broker_rate * 1_000_000.0).round() as i128;
            let broker_fee = IskCents((((max_order_cost.0 as i128) * broker_ppm + 500_000) / 1_000_000) as i64);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                // 1. Pre-check caller balance for worst-case debit using checked arithmetic
                let required_upfront = if is_bid {
                    max_order_cost.checked_add(broker_fee).ok_or_else(|| BridgeRpcError { code: "MarketOrderOverflow".into(), message: "Order cost + broker fee overflow".into() })?
                } else {
                    broker_fee
                };

                let caller_balance = snap.characters.get(&char_id).map(|c| c.balance).unwrap_or(IskCents(0));
                if caller_balance.checked_sub(required_upfront).is_none() {
                    return Err(BridgeRpcError { code: "InsufficientFunds".into(), message: "Not enough ISK for market order and broker fee".into() });
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
                                is_blueprint_copy: false,
                                blueprint_runs_remaining: None,
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
                                    is_blueprint_copy: false,
                                    blueprint_runs_remaining: None,
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

        ("marketProxy", "BuyMultipleItems") => {
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
                        snap.next_entity_id += 1;
                        let jid = snap.next_entity_id;
                        snap.wallet_journal.push(WalletJournalEntry {
                            entry_id: jid,
                            character_id: char_id,
                            entry_type: "MarketTransaction".into(),
                            amount: fill_gross,
                            balance_after: bal_after,
                            reason: format!("Bought {fill_qty}x type {fill_tid}"),
                        });
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
                            dynamic_attributes: HashMap::new(),
                            source_mutaplasmid_id: None,
                            insured_payout_isk: 0.0,
                            is_blueprint_copy: false,
                            blueprint_runs_remaining: None,
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

        // Contracts (`contractProxy`): ItemExchange & Courier (`E2E-15`)
        ("contractProxy", "CreateContract") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let contract_type = args
                .first()
                .and_then(|a| a.as_str())
                .unwrap_or("Courier");
            if !matches!(contract_type, "Courier" | "ItemExchange") {
                return Some(Err(BridgeRpcError {
                    code: "InvalidContractType".into(),
                    message: format!(
                        "Unsupported contract type '{contract_type}'; must be Courier or ItemExchange"
                    ),
                }));
            }
            let Some(start_station_id) = args
                .get(1)
                .and_then(|a| a.as_i64())
                .or(session.station_id)
                .or(session.structure_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDockedAtStartStation".into(),
                    message: "Caller must be docked at start_station_id to create a contract"
                        .into(),
                }));
            };
            let end_station_id = match args.get(2).and_then(|a| a.as_i64()) {
                Some(eid) => eid,
                None if contract_type == "ItemExchange" => start_station_id,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidContractDestination".into(),
                        message: "Courier contracts require an explicit destination stationID"
                            .into(),
                    }));
                }
            };
            let reward_isk = args.get(3).and_then(|a| a.as_f64()).unwrap_or(0.0);
            let collateral_isk = args.get(4).and_then(|a| a.as_f64()).unwrap_or(0.0);
            let item_id_opt = args.get(5).and_then(|a| a.as_i64());

            if session.station_id.or(session.structure_id) != Some(start_station_id) {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDockedAtStartStation".into(),
                    message: format!(
                        "Caller must be docked at start_station_id {start_station_id} to create a contract"
                    ),
                }));
            }

            if contract_type == "ItemExchange" {
                if end_station_id != start_station_id {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidContractDestination".into(),
                        message: format!(
                            "{contract_type} contracts cannot relocate items to a different destination station ({start_station_id} != {end_station_id})"
                        ),
                    }));
                }
                if item_id_opt.is_none() {
                    return Some(Err(BridgeRpcError {
                        code: "ContractRequiresItem".into(),
                        message: format!(
                            "{contract_type} contracts require an explicit item_id in station hangar"
                        ),
                    }));
                }
            }

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

            let start_sde_valid = ctx
                .sde
                .solar_systems
                .values()
                .any(|s| s.stations.iter().any(|st| st.station_id == start_station_id));
            let end_sde_valid = ctx
                .sde
                .solar_systems
                .values()
                .any(|s| s.stations.iter().any(|st| st.station_id == end_station_id));
            let now_unix = ctx.now_unix();
            let active_ship = session.ship_id;

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let start_valid = start_sde_valid
                    || snap
                        .structures
                        .get(&start_station_id)
                        .map(|st| st.state != "Destroyed")
                        .unwrap_or(false);
                let end_valid = end_sde_valid
                    || snap
                        .structures
                        .get(&end_station_id)
                        .map(|st| st.state != "Destroyed")
                        .unwrap_or(false);
                if !start_valid || !end_valid {
                    return Err(BridgeRpcError {
                        code: "InvalidContractStation".into(),
                        message: format!(
                            "Start station {start_station_id} or destination station {end_station_id} is not a valid station or active structure"
                        ),
                    });
                }

                let reward = IskCents::from_f64_isk(reward_isk);
                let collateral = IskCents::from_f64_isk(collateral_isk);
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                if contract_type == "Courier" {
                    let new_bal =
                        ch.balance
                            .checked_sub(reward)
                            .ok_or_else(|| BridgeRpcError {
                                code: "InsufficientFunds".into(),
                                message: "Not enough ISK to escrow contract reward".into(),
                            })?;
                    ch.balance = new_bal;
                }

                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                let mut escrowed_items = Vec::new();
                if let Some(iid) = item_id_opt {
                    if Some(iid) == active_ship
                        || snap
                            .characters
                            .get(&char_id)
                            .map(|c| c.active_ship_id == iid)
                            .unwrap_or(false)
                    {
                        return Err(BridgeRpcError {
                            code: "CannotContractActiveShip".into(),
                            message: "Cannot escrow the currently boarded active ship into a contract".into(),
                        });
                    }
                    let has_contained = snap
                        .items
                        .values()
                        .any(|other| other.item_id != iid && other.location_id == iid && other.quantity > 0);
                    if has_contained {
                        return Err(BridgeRpcError {
                            code: "CannotContractNonEmptyContainer".into(),
                            message: format!(
                                "Cannot escrow item {iid} into a contract while it contains fitted modules, rigs, or cargo"
                            ),
                        });
                    }
                    let already_locked = snap.is_item_locked_in_contract_or_job(iid);
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
                    if item.flag == InventoryFlag::Locked || already_locked {
                        return Err(BridgeRpcError {
                            code: "ContractItemLocked".into(),
                            message: format!(
                                "Item {iid} is currently locked in an active industry job or contract"
                            ),
                        });
                    }
                    item.location_id = id;
                    item.flag = InventoryFlag::Locked;
                    item.version += 1;
                    escrowed_items.push(iid);
                } else if contract_type == "Courier" {
                    snap.next_entity_id += 1;
                    let wrap_id = snap.next_entity_id;
                    snap.items.insert(
                        wrap_id,
                        InventoryItemRecord {
                            item_id: wrap_id,
                            type_id: 24445, // Courier Plastic Wrap (temporary envelope removed on delivery)
                            owner_id: char_id,
                            location_id: id,
                            flag: InventoryFlag::Locked,
                            quantity: 1,
                            singleton: true,
                            version: 1,
                            dynamic_attributes: HashMap::new(),
                            source_mutaplasmid_id: None,
                            insured_payout_isk: 0.0,
                            is_blueprint_copy: false,
                            blueprint_runs_remaining: None,
                        },
                    );
                    escrowed_items.push(wrap_id);
                }

                let price = if contract_type == "Courier" {
                    IskCents(0)
                } else {
                    reward
                };
                let courier_reward = if contract_type == "Courier" {
                    reward
                } else {
                    IskCents(0)
                };

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
                        price,
                        reward: courier_reward,
                        collateral: if contract_type == "Courier" {
                            collateral
                        } else {
                            IskCents(0)
                        },
                        item_ids: escrowed_items,
                        expiration_unix: now_unix.saturating_add(86_400),
                    },
                );
                Ok((id, now_unix.saturating_add(86_400)))
            });

            match res {
                Ok((cid, exp_unix)) => Some(Ok(PyRep::keyval([
                    ("contractID", PyRep::Integer(cid)),
                    ("status", PyRep::String("Outstanding".into())),
                    ("expirationUnix", PyRep::Integer(exp_unix as i64)),
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
            let caller_station = session.station_id.or(session.structure_id);
            let caller_ship = session.ship_id;
            let now_unix = ctx.now_unix();
            // If contract is expired while Outstanding, durably refund escrowed reward and return items before returning ContractExpired
            let expired_resolved = ctx
                .db
                .try_with_mut_snapshot(|snap| Ok::<_, std::convert::Infallible>(snap.resolve_expired_contract(cid, now_unix)))
                .unwrap();
            if expired_resolved.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ContractExpired".into(),
                    message: format!(
                        "Contract {cid} has expired; escrowed items and reward returned to issuer"
                    ),
                }));
            }
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
                if !matches!(c.contract_type.as_str(), "Courier" | "ItemExchange") {
                    return Err(BridgeRpcError {
                        code: "InvalidContractType".into(),
                        message: format!(
                            "Unsupported contract type '{}'; only Courier and ItemExchange are supported",
                            c.contract_type
                        ),
                    });
                }
                if c.issuer_id == char_id {
                    return Err(BridgeRpcError {
                        code: "CannotAcceptOwnContract".into(),
                        message: "Issuer cannot accept their own contract".into(),
                    });
                }
                if caller_station != Some(c.start_station_id) {
                    return Err(BridgeRpcError {
                        code: "MustBeDockedAtStartStation".into(),
                        message: format!(
                            "Accepting contract {cid} requires being docked at start station {}",
                            c.start_station_id
                        ),
                    });
                }
                // Verify all escrowed items still exist and are locked in contract escrow BEFORE transferring any ISK
                for iid in &c.item_ids {
                    let valid_escrow = snap
                        .items
                        .get(iid)
                        .map(|item| item.location_id == cid && item.flag == InventoryFlag::Locked)
                        .unwrap_or(false);
                    if !valid_escrow {
                        return Err(BridgeRpcError {
                            code: "ContractEscrowItemMissing".into(),
                            message: format!(
                                "Escrowed item {iid} is missing or no longer locked in contract {cid}"
                            ),
                        });
                    }
                }
                if c.contract_type == "Courier" && caller_ship.is_none() {
                    return Err(BridgeRpcError {
                        code: "NoActiveShip".into(),
                        message: "Accepting a Courier contract requires an active ship to load the courier package".into(),
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
                if c.contract_type == "ItemExchange" {
                    for iid in &c.item_ids {
                        if let Some(item) = snap.items.get_mut(iid) {
                            item.owner_id = char_id;
                            item.location_id = c.start_station_id;
                            item.flag = InventoryFlag::Hangar;
                            item.version += 1;
                        }
                    }
                    if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                        c_mut.acceptor_id = Some(char_id);
                        c_mut.status = "Completed".into();
                    }
                    return Ok(("Completed", c.expiration_unix));
                }
                let ship_id = caller_ship.unwrap();
                for iid in &c.item_ids {
                    if let Some(item) = snap.items.get_mut(iid) {
                        item.location_id = ship_id;
                        item.flag = InventoryFlag::Locked;
                        item.version += 1;
                    }
                }
                let delivery_deadline = now_unix.saturating_add(86_400);
                if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                    c_mut.acceptor_id = Some(char_id);
                    c_mut.status = "InProgress".into();
                    c_mut.expiration_unix = delivery_deadline;
                }
                Ok(("InProgress", delivery_deadline))
            });
            match res {
                Ok((new_status, exp_unix)) => Some(Ok(PyRep::keyval([
                    ("contractID", PyRep::Integer(cid)),
                    ("status", PyRep::String(new_status.into())),
                    ("expirationUnix", PyRep::Integer(exp_unix as i64)),
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
            let caller_ship = session.ship_id;
            let now_unix = ctx.now_unix();
            // If Courier contract expired while InProgress, durably settle collateral + reward to issuer and mark Failed
            let expired_resolved = ctx
                .db
                .try_with_mut_snapshot(|snap| Ok::<_, std::convert::Infallible>(snap.resolve_expired_contract(cid, now_unix)))
                .unwrap();
            if expired_resolved.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ContractExpired".into(),
                    message: format!(
                        "Courier contract {cid} has expired; collateral and escrowed reward settled to issuer"
                    ),
                }));
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(c) = snap.contracts.get(&cid).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ContractNotFound".into(),
                        message: format!("Contract {cid} does not exist"),
                    });
                };
                if c.contract_type != "Courier" {
                    return Err(BridgeRpcError {
                        code: "InvalidContractType".into(),
                        message: format!(
                            "Only Courier contracts can be completed via CompleteContract (got '{}')",
                            c.contract_type
                        ),
                    });
                }
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
                if caller_station != Some(c.end_station_id) {
                    return Err(BridgeRpcError {
                        code: "CourierNotAtDestinationStation".into(),
                        message: format!(
                            "Completing Courier contract {cid} requires being docked at destination station {}",
                            c.end_station_id
                        ),
                    });
                }
                let Some(ship_id) = caller_ship else {
                    return Err(BridgeRpcError {
                        code: "NoActiveShip".into(),
                        message: "Completing Courier contract requires the ship carrying the courier package".into(),
                    });
                };
                for iid in &c.item_ids {
                    let present = snap
                        .items
                        .get(iid)
                        .map(|item| {
                            item.location_id == ship_id || item.location_id == c.end_station_id
                        })
                        .unwrap_or(false);
                    if !present {
                        return Err(BridgeRpcError {
                            code: "CourierPackageMissing".into(),
                            message: format!(
                                "Courier item {iid} is not present in active ship {ship_id} or destination station {}",
                                c.end_station_id
                            ),
                        });
                    }
                }
                for iid in &c.item_ids {
                    let is_temp_wrap = snap
                        .items
                        .get(iid)
                        .map(|item| item.type_id == 24445)
                        .unwrap_or(false);
                    if is_temp_wrap {
                        snap.items.remove(iid);
                    } else if let Some(item) = snap.items.get_mut(iid) {
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

        ("contractProxy", "DeleteContract") | ("contractProxy", "FailContract") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let cid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(c) = snap.contracts.get(&cid).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ContractNotFound".into(),
                        message: format!("Contract {cid} does not exist"),
                    });
                };
                if c.issuer_id != char_id && c.acceptor_id != Some(char_id) {
                    return Err(BridgeRpcError {
                        code: "ContractOwnershipViolation".into(),
                        message: format!(
                            "Character {char_id} is neither issuer nor acceptor of contract {cid}"
                        ),
                    });
                };
                if let Some(new_state) = snap.resolve_expired_contract(cid, now_unix) {
                    return Ok(new_state);
                }
                if method == "DeleteContract" && c.status == "Outstanding" && c.issuer_id == char_id {
                    if c.contract_type == "Courier" && c.reward.0 > 0 {
                        if let Some(issuer) = snap.characters.get_mut(&c.issuer_id) {
                            issuer.balance = issuer
                                .balance
                                .checked_add(c.reward)
                                .unwrap_or(issuer.balance);
                        }
                    }
                    for iid in &c.item_ids {
                        let is_temp_wrap = snap
                            .items
                            .get(iid)
                            .map(|item| item.type_id == 24445)
                            .unwrap_or(false);
                        if is_temp_wrap {
                            snap.items.remove(iid);
                        } else if let Some(item) = snap.items.get_mut(iid) {
                            item.owner_id = c.issuer_id;
                            item.location_id = c.start_station_id;
                            item.flag = InventoryFlag::Hangar;
                            item.version += 1;
                        }
                    }
                    if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                        c_mut.status = "Deleted".into();
                    }
                    return Ok("Deleted");
                }
                if method == "FailContract"
                    && c.status == "InProgress"
                    && c.contract_type == "Courier"
                    && c.acceptor_id == Some(char_id)
                {
                    let payout = c
                        .collateral
                        .checked_add(c.reward)
                        .unwrap_or(c.collateral);
                    if payout.0 > 0 {
                        if let Some(issuer) = snap.characters.get_mut(&c.issuer_id) {
                            issuer.balance = issuer
                                .balance
                                .checked_add(payout)
                                .unwrap_or(issuer.balance);
                        }
                    }
                    for iid in &c.item_ids {
                        let is_temp_wrap = snap
                            .items
                            .get(iid)
                            .map(|item| item.type_id == 24445)
                            .unwrap_or(false);
                        let in_ship_container = snap
                            .items
                            .get(iid)
                            .map(|item| snap.items.contains_key(&item.location_id))
                            .unwrap_or(false);
                        if is_temp_wrap {
                            snap.items.remove(iid);
                        } else if let Some(item) = snap.items.get_mut(iid) {
                            if c.collateral.0 > 0 {
                                item.owner_id = char_id;
                                item.flag = if in_ship_container {
                                    InventoryFlag::Cargo
                                } else {
                                    InventoryFlag::Hangar
                                };
                            } else {
                                item.owner_id = c.issuer_id;
                                item.location_id = c.start_station_id;
                                item.flag = InventoryFlag::Hangar;
                            }
                            item.version += 1;
                        }
                    }
                    if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                        c_mut.status = "Failed".into();
                    }
                    return Ok("Failed");
                }
                Err(BridgeRpcError {
                    code: "InvalidContractState".into(),
                    message: format!(
                        "Contract {cid} in state '{}' cannot be resolved via {method} before expiration",
                        c.status
                    ),
                })
            });
            match res {
                Ok(new_status) => Some(Ok(PyRep::keyval([
                    ("contractID", PyRep::Integer(cid)),
                    ("status", PyRep::String(new_status.into())),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        _ => None,
    }
}
