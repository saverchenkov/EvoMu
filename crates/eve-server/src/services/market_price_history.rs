//! `market_price_history` — Regional daily OHLC market price history query builders.
//!
//! Formats daily aggregates from `snap.market_history` into the canonical client
//! rowset (`historyDate, lowPrice, highPrice, avgPrice, volume, orders`) expected
//! by `marketProxy.GetOldPriceHistory`, `GetNewPriceHistory`, and `GetHistoryForManyTypeIDs`.

use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TypeId, WorldStateSnapshot};
use eve_proto::{BridgeRpcError, PyRep};
use super::market_contracts::{compute_sales_tax_cents, PLEX_TYPE_ID};
use super::market_trading::{load_market_order_meta, save_market_order_meta, MarketOrderMeta};

pub(crate) struct ModifyCrossResult {
    pub delivered_to_corp_deliveries: i32,
    pub final_escrow: IskCents,
}

/// Executes crossed order fills when a market order price is modified via `ModifyCharOrder`.
/// Handles buy orders crossing resting asks (including margin trading shortfall debit and escrow refund)
/// and sell orders crossing resting bids (including buyer margin shortfall coverage and seller proceeds).
pub(crate) fn execute_crossed_order_fills_on_modify(
    ctx: &ServerContext,
    snap: &mut WorldStateSnapshot,
    order_id: i64,
    new_price: IskCents,
    escrow_fraction: f64,
    is_corp_order: bool,
    caller_corp_id: i64,
    char_id: i64,
    now_unix: u64,
    meta: &mut MarketOrderMeta,
) -> Result<ModifyCrossResult, BridgeRpcError> {
    let order = match snap.market_orders.get(&order_id).cloned() {
        Some(o) => o,
        None => return Ok(ModifyCrossResult { delivered_to_corp_deliveries: 0, final_escrow: IskCents(0) }),
    };

    let tax_collector = crate::services::wallet_serverops::tax_collector_id(ctx);
    let mut delivered_to_corp_deliveries = 0_i32;

    if order.is_bid {
        // Buy order modified upward: crosses resting ask orders at same station
        let mut ask_ids: Vec<(i64, IskCents)> = snap.market_orders.values().filter(|o| {
            !o.is_bid && o.station_id == order.station_id && o.type_id == order.type_id
                && o.vol_remaining > 0 && o.price.0 <= new_price.0 && o.owner_id != order.owner_id
        }).map(|o| (o.order_id, o.price)).collect();
        ask_ids.sort_by(|a, b| a.1.0.cmp(&b.1.0).then_with(|| a.0.cmp(&b.0)));

        let buyer_owner_id = order.owner_id;

        for (ask_id, _) in ask_ids {
            let cur_rem = snap.market_orders.get(&order_id).map(|o| o.vol_remaining).unwrap_or(0);
            if cur_rem <= 0 { break; }
            let Some(ask) = snap.market_orders.get(&ask_id).cloned() else { continue; };
            let fill_qty = cur_rem.min(ask.vol_remaining);
            if fill_qty <= 0 { continue; }
            let fill_cost = ask.price.checked_mul_qty(fill_qty as i64).unwrap_or(IskCents(0));

            // Seller proceeds and taxes
            let (mut seller_net, mut seller_bal, mut fill_tax) = (IskCents(0), None, IskCents(0));
            if !ask.is_npc {
                let seller_meta = load_market_order_meta(snap, &ask, now_unix);
                let seller_acct_lvl = snap.skills.get(&ask.owner_id).and_then(|m| m.get(&16622)).map(|s| s.level).unwrap_or(0);
                fill_tax = compute_sales_tax_cents(fill_cost, seller_acct_lvl);
                seller_net = fill_cost.checked_sub(fill_tax).unwrap_or(IskCents(0));
                if let Some(seller) = snap.characters.get_mut(&ask.owner_id) {
                    seller.balance = seller.balance.checked_add(seller_net).unwrap_or(seller.balance);
                    seller_bal = Some(seller.balance);
                } else if let Some(seller_corp) = snap.corporations.get_mut(&ask.owner_id) {
                    seller_corp.master_wallet = seller_corp.master_wallet.checked_add(seller_net).unwrap_or(seller_corp.master_wallet);
                    let s_div = seller_meta.account_key.map(|k| (k.saturating_sub(999)).clamp(1, 7) as u8).unwrap_or(1);
                    seller_corp.ensure_wallet_divisions();
                    if let Some(d) = seller_corp.wallet_divisions.iter_mut().find(|d| d.division_id == s_div) {
                        d.balance = d.balance.checked_add(seller_net).unwrap_or(d.balance);
                    }
                    seller_bal = Some(seller_corp.master_wallet);
                }
            }

            // Buyer escrow release, price improvement refund, or margin shortfall debit
            let escrow_per_unit = ((new_price.0 as f64) * escrow_fraction).round() as i64;
            let current_escrow = snap.market_orders.get(&order_id).map(|o| o.escrow.0).unwrap_or(0);
            let escrow_released = (escrow_per_unit * (fill_qty as i64)).min(current_escrow);

            if escrow_released > fill_cost.0 {
                let refund = IskCents(escrow_released - fill_cost.0);
                if is_corp_order {
                    if let Some(corp_mut) = snap.corporations.get_mut(&caller_corp_id) {
                        corp_mut.ensure_wallet_divisions();
                        corp_mut.master_wallet = corp_mut.master_wallet.checked_add(refund).unwrap_or(corp_mut.master_wallet);
                        let div_id = meta.account_key.map(|ak| (ak.saturating_sub(999)).clamp(1, 7) as u8).unwrap_or(1);
                        if let Some(d) = corp_mut.wallet_divisions.iter_mut().find(|d| d.division_id == div_id) {
                            d.balance = d.balance.checked_add(refund).unwrap_or(d.balance);
                        }
                    }
                } else if let Some(buyer) = snap.characters.get_mut(&char_id) {
                    buyer.balance = buyer.balance.checked_add(refund).unwrap_or(buyer.balance);
                }
            } else if fill_cost.0 > escrow_released {
                let uncovered = IskCents(fill_cost.0 - escrow_released);
                if is_corp_order {
                    let div_id = meta.account_key.map(|ak| (ak.saturating_sub(999)).clamp(1, 7) as u8).unwrap_or(1);
                    let corp_mut = snap.corporations.get_mut(&caller_corp_id).ok_or_else(|| BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: "Corporation not found".into(),
                    })?;
                    corp_mut.ensure_wallet_divisions();
                    let div_bal = corp_mut.wallet_divisions.iter().find(|d| d.division_id == div_id).map(|d| d.balance).unwrap_or(corp_mut.master_wallet);
                    if div_bal.0 < uncovered.0 || corp_mut.master_wallet.0 < uncovered.0 {
                        return Err(BridgeRpcError {
                            code: "InsufficientFunds".into(),
                            message: "Character does not have sufficient funds to cover margin shortfall".into(),
                        });
                    }
                    if let Some(div) = corp_mut.wallet_divisions.iter_mut().find(|d| d.division_id == div_id) {
                        div.balance = div.balance.checked_sub(uncovered).unwrap_or(IskCents(0));
                    }
                    corp_mut.master_wallet = corp_mut.master_wallet.checked_sub(uncovered).unwrap_or(IskCents(0));
                    let bal_after = corp_mut.master_wallet;
                    snap.append_journal(caller_corp_id, "MarketEscrowShortfall", uncovered, bal_after, format!("Margin shortfall for fill of order {order_id}"));
                } else {
                    let cur_bal = snap.characters.get(&char_id).map(|c| c.balance).unwrap_or(IskCents(0));
                    if cur_bal.0 < uncovered.0 {
                        return Err(BridgeRpcError {
                            code: "InsufficientFunds".into(),
                            message: "Character does not have sufficient funds to cover margin shortfall".into(),
                        });
                    }
                    let ch_mut = snap.characters.get_mut(&char_id).unwrap();
                    ch_mut.balance = IskCents(ch_mut.balance.0 - uncovered.0);
                    let bal_after = ch_mut.balance;
                    snap.append_journal_entry(eve_db::ledger::JournalEntryInput {
                        owner_id: char_id,
                        entry_type: "MarketEscrowShortfall",
                        amount: IskCents(-uncovered.0),
                        balance_after: bal_after,
                        reason: format!("Margin shortfall for fill of order {order_id}"),
                        meta: eve_db::ledger::JournalMeta::new(eve_db::ledger::ref_types::MARKET_ESCROW, char_id, 0, now_unix)
                            .with_context(eve_db::ledger::JournalContext::MarketOrder, order_id),
                    });
                }
            }

            snap.journal_market_fill(eve_db::ledger::MarketFill {
                buyer_id: buyer_owner_id, seller_id: ask.owner_id, type_id: order.type_id,
                quantity: fill_qty as i64, price: ask.price, location_id: order.station_id,
                date_unix: now_unix, gross: fill_cost, buyer_balance_after: None, seller_net,
                seller_balance_after: seller_bal, sales_tax: fill_tax, tax_collector_id: tax_collector,
                region_id: Some(order.region_id),
            });

            if let Some(a_mut) = snap.market_orders.get_mut(&ask_id) {
                a_mut.vol_remaining -= fill_qty;
                if a_mut.vol_remaining == 0 && !a_mut.is_npc {
                    let mut r_meta = load_market_order_meta(snap, &ask, now_unix);
                    r_meta.order_state = Some(2);
                    r_meta.final_vol_remaining = Some(0);
                    r_meta.closed_unix = Some(now_unix);
                    save_market_order_meta(snap, ask_id, r_meta);
                }
            }

            if let Some(b_mut) = snap.market_orders.get_mut(&order_id) {
                b_mut.vol_remaining -= fill_qty;
                b_mut.escrow = IskCents(b_mut.escrow.0 - escrow_released);
                if b_mut.vol_remaining == 0 && !b_mut.is_npc {
                    meta.order_state = Some(2);
                    meta.final_vol_remaining = Some(0);
                    meta.closed_unix = Some(now_unix);
                    save_market_order_meta(snap, order_id, *meta);
                }
            }

            if order.type_id == PLEX_TYPE_ID {
                let _ = snap.adjust_plex_vault(buyer_owner_id, fill_qty as i64);
            } else {
                snap.next_entity_id += 1;
                let delivery_item_id = snap.next_entity_id;
                let flag = if is_corp_order { InventoryFlag::Deliveries } else { InventoryFlag::Hangar };
                snap.items.insert(
                    delivery_item_id,
                    InventoryItemRecord {
                        item_id: delivery_item_id, type_id: order.type_id, owner_id: buyer_owner_id,
                        location_id: order.station_id, flag, quantity: fill_qty, singleton: false,
                        version: 1, ..Default::default()
                    },
                );
            }
            if is_corp_order { delivered_to_corp_deliveries += fill_qty; }
        }
    } else {
        // Sell order modified downward: crosses resting bid orders in range
        let mut bid_ids: Vec<(i64, IskCents)> = snap.market_orders.values().filter(|o| {
            o.is_bid && o.region_id == order.region_id && o.type_id == order.type_id
                && o.vol_remaining > 0 && o.price.0 >= new_price.0 && o.owner_id != order.owner_id
                && super::market_trading::can_bid_match_seller_station(ctx, snap, o, order.station_id, now_unix)
        }).map(|o| (o.order_id, o.price)).collect();
        bid_ids.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(&b.0)));

        let seller_owner_id = order.owner_id;
        let seller_sk = snap.skills.get(&char_id);
        let seller_acct_lvl = seller_sk.and_then(|m| m.get(&16622)).map(|s| s.level).unwrap_or(0);

        for (bid_id, _) in bid_ids {
            let cur_rem = snap.market_orders.get(&order_id).map(|o| o.vol_remaining).unwrap_or(0);
            if cur_rem <= 0 { break; }
            let Some(bid) = snap.market_orders.get(&bid_id).cloned() else { continue; };
            let required_min = bid.min_volume.max(1).min(bid.vol_remaining);
            if cur_rem < required_min { continue; }
            let fill_qty = cur_rem.min(bid.vol_remaining);
            if fill_qty <= 0 { continue; }
            let fill_cost = bid.price.checked_mul_qty(fill_qty as i64).unwrap_or(IskCents(0));

            // Buyer escrow release and margin shortfall coverage
            let from_escrow = bid.escrow.min(fill_cost);
            let uncovered = fill_cost.checked_sub(from_escrow).unwrap_or(IskCents(0));
            let b_meta = load_market_order_meta(snap, &bid, now_unix);
            let b_div_id = b_meta
                .account_key
                .map(|ak| (ak.saturating_sub(999)).clamp(1, 7) as u8)
                .unwrap_or(1);
            if !bid.is_npc && uncovered.0 > 0 {
                let covered = if let Some(buyer_ch) = snap.characters.get_mut(&bid.owner_id) {
                    if let Some(new_bal) = buyer_ch.balance.checked_sub(uncovered) {
                        buyer_ch.balance = new_bal;
                        snap.append_journal(bid.owner_id, "MarketEscrowCover", uncovered, new_bal, format!("Covered margin escrow shortfall for buy order {bid_id}"));
                        true
                    } else { false }
                } else if let Some(buyer_corp) = snap.corporations.get_mut(&bid.owner_id) {
                    buyer_corp.ensure_wallet_divisions();
                    let div_bal = buyer_corp
                        .wallet_divisions
                        .iter()
                        .find(|d| d.division_id == b_div_id)
                        .map(|d| d.balance)
                        .unwrap_or(buyer_corp.master_wallet);
                    if div_bal.0 >= uncovered.0 && buyer_corp.master_wallet.0 >= uncovered.0 {
                        buyer_corp.master_wallet = buyer_corp.master_wallet.checked_sub(uncovered).unwrap_or(IskCents(0));
                        if let Some(div) = buyer_corp.wallet_divisions.iter_mut().find(|d| d.division_id == b_div_id) {
                            div.balance = div.balance.checked_sub(uncovered).unwrap_or(IskCents(0));
                        }
                        let new_bal = buyer_corp.master_wallet;
                        snap.append_journal(bid.owner_id, "MarketEscrowCover", uncovered, new_bal, format!("Covered margin escrow shortfall for buy order {bid_id}"));
                        true
                    } else { false }
                } else { false };

                if !covered {
                    if bid.escrow.0 > 0 {
                        if let Some(buyer_ch) = snap.characters.get_mut(&bid.owner_id) {
                            buyer_ch.balance = buyer_ch.balance.checked_add(bid.escrow).unwrap_or(buyer_ch.balance);
                        } else if let Some(buyer_corp) = snap.corporations.get_mut(&bid.owner_id) {
                            buyer_corp.ensure_wallet_divisions();
                            buyer_corp.master_wallet = buyer_corp.master_wallet.checked_add(bid.escrow).unwrap_or(buyer_corp.master_wallet);
                            if let Some(div) = buyer_corp.wallet_divisions.iter_mut().find(|d| d.division_id == b_div_id) {
                                div.balance = div.balance.checked_add(bid.escrow).unwrap_or(div.balance);
                            }
                        }
                    }
                    if let Some(o_mut) = snap.market_orders.get_mut(&bid_id) {
                        o_mut.vol_remaining = 0;
                        o_mut.escrow = IskCents(0);
                    }
                    continue;
                }
            }

            // Seller proceeds & sales tax
            let fill_tax = compute_sales_tax_cents(fill_cost, seller_acct_lvl);
            let seller_net = fill_cost.checked_sub(fill_tax).unwrap_or(IskCents(0));
            let mut seller_bal = None;
            if is_corp_order {
                if let Some(corp_mut) = snap.corporations.get_mut(&caller_corp_id) {
                    corp_mut.ensure_wallet_divisions();
                    corp_mut.master_wallet = corp_mut.master_wallet.checked_add(seller_net).unwrap_or(corp_mut.master_wallet);
                    let div_id = meta.account_key.map(|ak| (ak.saturating_sub(999)).clamp(1, 7) as u8).unwrap_or(1);
                    if let Some(d) = corp_mut.wallet_divisions.iter_mut().find(|d| d.division_id == div_id) {
                        d.balance = d.balance.checked_add(seller_net).unwrap_or(d.balance);
                    }
                    seller_bal = Some(corp_mut.master_wallet);
                }
            } else if let Some(seller) = snap.characters.get_mut(&char_id) {
                seller.balance = seller.balance.checked_add(seller_net).unwrap_or(seller.balance);
                seller_bal = Some(seller.balance);
            }

            snap.journal_market_fill(eve_db::ledger::MarketFill {
                buyer_id: bid.owner_id, seller_id: seller_owner_id, type_id: order.type_id,
                quantity: fill_qty as i64, price: bid.price, location_id: order.station_id,
                date_unix: now_unix, gross: fill_cost, buyer_balance_after: None, seller_net,
                seller_balance_after: seller_bal, sales_tax: fill_tax, tax_collector_id: tax_collector,
                region_id: Some(order.region_id),
            });

            if let Some(b_mut) = snap.market_orders.get_mut(&bid_id) {
                b_mut.vol_remaining -= fill_qty;
                b_mut.escrow = b_mut.escrow.checked_sub(from_escrow).unwrap_or(IskCents(0));
                if b_mut.vol_remaining == 0 && !bid.is_npc {
                    let mut r_meta = load_market_order_meta(snap, &bid, now_unix);
                    r_meta.order_state = Some(2);
                    r_meta.final_vol_remaining = Some(0);
                    r_meta.closed_unix = Some(now_unix);
                    save_market_order_meta(snap, bid_id, r_meta);
                }
            }

            if let Some(s_mut) = snap.market_orders.get_mut(&order_id) {
                s_mut.vol_remaining -= fill_qty;
                if s_mut.vol_remaining == 0 && !s_mut.is_npc {
                    meta.order_state = Some(2);
                    meta.final_vol_remaining = Some(0);
                    meta.closed_unix = Some(now_unix);
                    save_market_order_meta(snap, order_id, *meta);
                }
            }

            if order.type_id == PLEX_TYPE_ID {
                let _ = snap.adjust_plex_vault(bid.owner_id, fill_qty as i64);
            } else {
                snap.next_entity_id += 1;
                let delivery_item_id = snap.next_entity_id;
                let buyer_is_corp = snap.corporations.contains_key(&bid.owner_id);
                let flag = if buyer_is_corp { InventoryFlag::Deliveries } else { InventoryFlag::Hangar };
                snap.items.insert(
                    delivery_item_id,
                    InventoryItemRecord {
                        item_id: delivery_item_id, type_id: order.type_id, owner_id: bid.owner_id,
                        location_id: order.station_id, flag, quantity: fill_qty, singleton: false,
                        version: 1, ..Default::default()
                    },
                );
            }
        }
    }

    let final_escrow = snap.market_orders.get(&order_id).map(|o| o.escrow).unwrap_or(IskCents(0));
    Ok(ModifyCrossResult { delivered_to_corp_deliveries, final_escrow })
}

/// Converts a "YYYY-MM-DD" date string to Unix epoch seconds (midnight UTC).
pub fn date_str_to_unix_secs(date_str: &str) -> Option<u64> {
    let mut parts = date_str.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe as i64 - 719_468;
    if days < 0 {
        return None;
    }
    Some((days as u64) * 86_400)
}

/// Converts a "YYYY-MM-DD" date string to Windows FileTime (100-nanosecond intervals since 1601-01-01).
pub fn date_str_to_filetime(date_str: &str) -> Option<i64> {
    let unix_secs = date_str_to_unix_secs(date_str)?;
    Some((unix_secs as i64 + 11_644_473_600) * 10_000_000)
}

/// Builds price history rows for a given `(region_id, type_id)` from persisted trade history.
/// Returns an empty vector if no trades exist for this type in the region.
pub fn build_price_history_rows(
    snap: &WorldStateSnapshot,
    type_id: TypeId,
    region_id: u32,
) -> Vec<Vec<PyRep>> {
    let mut rows = Vec::new();
    if let Some(entries) = snap.market_history.get(&(region_id, type_id)) {
        let mut sorted = entries.clone();
        sorted.sort_by(|a, b| a.date.cmp(&b.date));
        for day in sorted {
            let file_time = date_str_to_filetime(&day.date).unwrap_or(0);
            rows.push(vec![
                PyRep::Integer(file_time),
                PyRep::Real(day.lowest),
                PyRep::Real(day.highest),
                PyRep::Real(day.average),
                PyRep::Integer(day.volume),
                PyRep::Integer(day.order_count),
            ]);
        }
    } else {
        // Fallback: check if market_transactions have fills for this type in this region
        let mut daily_map: std::collections::BTreeMap<String, (f64, f64, i64, i64, f64)> = std::collections::BTreeMap::new();
        for tx in &snap.market_transactions {
            if tx.type_id == type_id {
                let tx_region = snap
                    .market_orders
                    .values()
                    .find(|o| o.station_id == tx.location_id)
                    .map(|o| o.region_id)
                    .unwrap_or(10_000_002);
                if tx_region == region_id {
                    let date_str = eve_db::ledger::unix_secs_to_date_str(tx.date_unix);
                    let unit_price = tx.price.to_wire_f64();
                    let qty = tx.quantity.max(1);
                    let entry = daily_map.entry(date_str).or_insert((unit_price, unit_price, 0, 0, 0.0));
                    entry.0 = entry.0.min(unit_price);
                    entry.1 = entry.1.max(unit_price);
                    entry.2 += qty;
                    entry.3 += 1;
                    entry.4 += unit_price * (qty as f64);
                }
            }
        }
        for (date_str, (low, high, vol, orders, total_val)) in daily_map {
            let avg = if vol > 0 { total_val / (vol as f64) } else { (low + high) / 2.0 };
            let file_time = date_str_to_filetime(&date_str).unwrap_or(0);
            rows.push(vec![
                PyRep::Integer(file_time),
                PyRep::Real(low),
                PyRep::Real(high),
                PyRep::Real(avg),
                PyRep::Integer(vol),
                PyRep::Integer(orders),
            ]);
        }
    }
    rows
}
