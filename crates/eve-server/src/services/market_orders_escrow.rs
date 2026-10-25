//! `market_orders_escrow` — Multi-Sell Orders, Order Cancellation, Multi-Buy Execution, & Price History (`E-1`, `E-2`).

#![forbid(unsafe_code)]

use super::{
    can_bid_match_seller_station, compute_sales_tax_cents, is_structure_market_online,
    load_market_order_meta, save_market_order_meta, structure_owner_broker_share, MarketOrderMeta,
};
use crate::formulas::compute_broker_fee_rate;
use crate::router::{NotificationTarget, ServerContext};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

pub fn handle_orders_escrow(
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
        // V24.01 `code.ccp` `eve/client/script/ui/services/marketsvc.pyj` (`SellMulti`) and
        // `eve/client/script/ui/shared/market/sellMulti.pyj` (`GetValidatedItem`):
        // `marketProxy.PlaceMultiSellOrder(itemList, useCorp, duration, expectedBrokersFee)`
        // where each entry in `itemList` is a KeyVal/dict with `(stationID, typeID, itemID, price, quantity[, officeID, delta, rawBrokerFeePercentage])`.
        "PlaceMultiSellOrder" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(docked_station_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NotDockedAtStation".into(),
                    message: "Character must be docked at a station or structure to PlaceMultiSellOrder".into(),
                }));
            };
            let Some(region_id) = session.region_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoRegion".into(),
                    message: "Session has no active region".into(),
                }));
            };
            let Some(PyRep::List(entries) | PyRep::Tuple(entries)) = args.first() else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMultiSellOrder".into(),
                    message: "PlaceMultiSellOrder requires itemList as first argument".into(),
                }));
            };
            if entries.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMultiSellOrder".into(),
                    message: "PlaceMultiSellOrder requires at least one item entry".into(),
                }));
            }
            let use_corp = args
                .get(1)
                .and_then(|a| a.as_bool())
                .or_else(|| _kwargs.get("useCorp").and_then(|v| v.as_bool()))
                .unwrap_or(false);
            let duration_days = args.get(2).and_then(|a| a.as_i64()).unwrap_or(90);
            if ![0, 1, 3, 7, 14, 30, 90].contains(&duration_days) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMarketOrder".into(),
                    message: format!(
                        "Invalid market order duration {duration_days}; expected one of [0, 1, 3, 7, 14, 30, 90]"
                    ),
                }));
            }
            let account_key = args
                .get(4)
                .and_then(|a| a.as_i64())
                .or_else(|| _kwargs.get("accountKey").and_then(|v| v.as_i64()))
                .unwrap_or(session.corp_account_key as i64)
                .clamp(1000, 1006) as u16;
            let expected_brokers_fee = match args.get(3) {
                Some(PyRep::None) | None => None,
                Some(v) => match v.as_f64() {
                    Some(f) if f.is_finite() && f > 0.0 => Some(IskCents::from_f64_isk(f)),
                    Some(f) if f == 0.0 => None,
                    _ => {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidMultiSellOrder".into(),
                            message: "PlaceMultiSellOrder expectedBrokersFee must be non-negative and finite".into(),
                        }));
                    }
                },
            };

            let station_info = ctx.sde.solar_systems.values().find_map(|sys| {
                if sys.region_id == region_id {
                    sys.stations
                        .iter()
                        .find(|st| st.station_id == docked_station_id)
                } else {
                    None
                }
            });
            let (owner_corp_id, owner_faction_id, is_structure) = if let Some(station_def) =
                station_info
            {
                (
                    station_def.owner_corporation_id,
                    station_def.owner_faction_id,
                    false,
                )
            } else if let Some((str_owner_corp, str_region, hub_online)) =
                ctx.db.with_snapshot(|snap| {
                    snap.structures.get(&docked_station_id).and_then(|st| {
                        ctx.sde
                            .solar_systems
                            .get(&st.solar_system_id)
                            .map(|sys| (st.owner_corp_id, sys.region_id, is_structure_market_online(snap, docked_station_id)))
                    })
                })
            {
                if str_region != region_id {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidMarketStation".into(),
                        message: format!(
                            "Structure {docked_station_id} is not in region {region_id}"
                        ),
                    }));
                }
                if !hub_online {
                    return Some(Err(BridgeRpcError {
                        code: "MarketHubOffline".into(),
                        message: format!("Standup Market Hub I service module at structure {docked_station_id} is offline"),
                    }));
                }
                (str_owner_corp, None, true)
            } else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMarketStation".into(),
                    message: format!("Station {docked_station_id} is not in region {region_id}"),
                }));
            };

            let mut parsed_items: Vec<(i64, TypeId, i64, IskCents, i32, i64)> =
                Vec::with_capacity(entries.len());
            for entry in entries {
                let (st_id, raw_tid, item_id, price_isk, qty, item_dur) = match entry {
                    PyRep::KeyVal(kv) => (
                        kv.get("stationID")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(docked_station_id),
                        kv.get("typeID").and_then(|v| v.as_i64()).unwrap_or(0),
                        kv.get("itemID").and_then(|v| v.as_i64()).unwrap_or(0),
                        kv.get("price").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        kv.get("quantity").and_then(|v| v.as_i64()).unwrap_or(0),
                        kv.get("duration").and_then(|v| v.as_i64()).unwrap_or(duration_days),
                    ),
                    PyRep::Dict(kv) => {
                        let find_key = |k: &str| {
                            kv.iter()
                                .find(|(ek, _)| ek.as_str() == Some(k))
                                .map(|(_, ev)| ev)
                        };
                        (
                            find_key("stationID")
                                .and_then(|e| e.as_i64())
                                .unwrap_or(docked_station_id),
                            find_key("typeID").and_then(|e| e.as_i64()).unwrap_or(0),
                            find_key("itemID").and_then(|e| e.as_i64()).unwrap_or(0),
                            find_key("price").and_then(|e| e.as_f64()).unwrap_or(0.0),
                            find_key("quantity").and_then(|e| e.as_i64()).unwrap_or(0),
                            find_key("duration").and_then(|e| e.as_i64()).unwrap_or(duration_days),
                        )
                    }
                    _ => {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidMultiSellOrder".into(),
                            message: "Each PlaceMultiSellOrder entry must be a KeyVal/dict with stationID, typeID, itemID, price, quantity".into(),
                        }));
                    }
                };
                if st_id != docked_station_id {
                    return Some(Err(BridgeRpcError {
                        code: "StationMismatch".into(),
                        message: format!(
                            "Cannot place sell order at station {st_id} while docked at {docked_station_id}"
                        ),
                    }));
                }
                if raw_tid <= 0
                    || !price_isk.is_finite()
                    || price_isk <= 0.0
                    || qty <= 0
                    || qty > (i32::MAX as i64)
                    || ![0, 1, 3, 7, 14, 30, 90].contains(&item_dur)
                {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidMultiSellOrder".into(),
                        message: "PlaceMultiSellOrder entry requires valid positive typeID, price, quantity, and duration".into(),
                    }));
                }
                if !super::super::market_contracts::is_valid_4_sig_fig_price(price_isk) {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidPriceTickSize".into(),
                        message: format!(
                            "Sell order price {price_isk} violates EVE 4-significant-figure tick size rule"
                        ),
                    }));
                }
                let tid = raw_tid as TypeId;
                if ctx.sde.types.get(&tid).and_then(|t| t.market_group_id).is_none() {
                    return Some(Err(BridgeRpcError {
                        code: "ItemNotMarketEligible".into(),
                        message: format!("Type {tid} has no marketGroupID and cannot be sold on the market"),
                    }));
                }
                parsed_items.push((
                    st_id,
                    tid,
                    item_id,
                    IskCents::from_f64_isk(price_isk),
                    qty as i32,
                    item_dur,
                ));
            }

            let now_unix = ctx.now_unix();
            let tax_collector = crate::services::wallet_serverops::tax_collector_id(ctx);
            // Execute the entire multi-sell batch atomically inside a single snapshot transaction
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let caller_corp_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0);
                let div_id = (account_key.saturating_sub(999)).clamp(1, 7) as u8;
                if use_corp {
                    let Some(corp) = snap.corporations.get_mut(&caller_corp_id) else {
                        return Err(BridgeRpcError {
                            code: "CorporationNotFound".into(),
                            message: format!(
                                "Corporation {caller_corp_id} not found for corporate sell order"
                            ),
                        });
                    };
                    let role_mask = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                    if corp.ceo_id != char_id
                        && !eve_db::corp_roles::has_role(
                            role_mask,
                            eve_db::corp_roles::ROLE_ACCOUNTANT,
                        )
                        && !eve_db::corp_roles::has_role(
                            role_mask,
                            eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT,
                        )
                        && !eve_db::corp_roles::has_role(
                            role_mask,
                            1u64 << 20, // corpRoleTrader
                        )
                    {
                        return Err(BridgeRpcError {
                            code: "CorpRoleRequired".into(),
                            message: "Character lacks corporation trader/accountant role to place corporate sell order".into(),
                        });
                    }
                    corp.ensure_wallet_divisions();
                }
                let order_owner_id = if use_corp { caller_corp_id } else { char_id };

                let sk = snap.skills.get(&char_id);
                let broker_lvl = sk.and_then(|m| m.get(&3446)).map(|s| s.level).unwrap_or(0);
                let acct_lvl = sk.and_then(|m| m.get(&16622)).map(|s| s.level).unwrap_or(0);
                let trade_lvl =
                    sk.and_then(|m| m.get(&3443)).map(|s| s.level).unwrap_or(0) as usize;
                let retail_lvl =
                    sk.and_then(|m| m.get(&3444)).map(|s| s.level).unwrap_or(0) as usize;
                let wholesale_lvl =
                    sk.and_then(|m| m.get(&16596)).map(|s| s.level).unwrap_or(0) as usize;
                let tycoon_lvl =
                    sk.and_then(|m| m.get(&18580)).map(|s| s.level).unwrap_or(0) as usize;
                let max_open_orders =
                    5 + 4 * trade_lvl + 8 * retail_lvl + 16 * wholesale_lvl + 32 * tycoon_lvl;
                let ch = snap.characters.get(&char_id);
                let faction_standing = ch
                    .and_then(|c| owner_faction_id.and_then(|fid| c.standings.get(&fid).copied()))
                    .unwrap_or(0.0);
                let corp_standing = ch
                    .and_then(|c| c.standings.get(&owner_corp_id).copied())
                    .unwrap_or(0.0);
                let broker_rate = if is_structure {
                    super::resolve_structure_broker_rate(snap, docked_station_id)
                } else {
                    compute_broker_fee_rate(broker_lvl, faction_standing, corp_standing)
                };
                let broker_ppm = (broker_rate * 1_000_000.0).round() as i128;

                let mut order_ids = Vec::with_capacity(parsed_items.len());
                let mut total_broker_fee = IskCents(0);
                let mut total_sales_tax = IskCents(0);
                let mut all_filled_immediately = true;
                let mut matched_buyers: Vec<(i64, i64)> = Vec::new();

                for (station_id, type_id, raw_item_id, order_price, quantity_i32, item_dur) in parsed_items {
                    let _ = order_price
                        .checked_mul_qty(quantity_i32 as i64)
                        .ok_or_else(|| BridgeRpcError {
                            code: "MarketOrderOverflow".into(),
                            message: "Order total cost overflows 64-bit ISK cents".into(),
                        })?;
                    let is_sellable_item = |snap_ref: &eve_db::WorldStateSnapshot, i: &InventoryItemRecord| {
                        snap_ref.is_item_consumable_by(i, char_id, Some(station_id), None)
                            || (use_corp && i.owner_id == caller_corp_id && i.location_id == station_id)
                    };
                    let is_courier_item = |iid: i64| snap.item_custom_names.get(&iid).map_or(false, |s| s.starts_with("courier_cargo:"));

                    // Resolve itemID (explicit or auto-discovered from hangar stack)
                    let item_id = if raw_item_id > 0 {
                        raw_item_id
                    } else {
                        let found_id = snap.items.values().find(|i| {
                            i.type_id == type_id
                                && i.quantity >= quantity_i32
                                && !i.singleton
                                && is_sellable_item(snap, i)
                                && !is_courier_item(i.item_id)
                                && !snap.items.values().any(|other| other.item_id != i.item_id && other.location_id == i.item_id && other.quantity > 0)
                        }).map(|i| i.item_id);
                        let Some(fid) = found_id else {
                            if snap.items.values().any(|i| i.type_id == type_id && is_sellable_item(snap, i) && is_courier_item(i.item_id)) {
                                return Err(BridgeRpcError {
                                    code: "ItemCannotBeSold".into(),
                                    message: "Courier mission items cannot be sold on the market".into(),
                                });
                            }
                            let has_non_empty_candidate = snap.items.values().any(|i| {
                                i.type_id == type_id
                                    && is_sellable_item(snap, i)
                                    && snap.items.values().any(|other| other.item_id != i.item_id && other.location_id == i.item_id && other.quantity > 0)
                            });
                            if has_non_empty_candidate {
                                return Err(BridgeRpcError {
                                    code: "CannotSellItemWithContents".into(),
                                    message: format!("Cannot sell ship or container of type {type_id} while it contains fitted modules, rigs, or cargo"),
                                });
                            }
                            let has_assembled_candidate = snap.items.values().any(|i| i.type_id == type_id && i.singleton && is_sellable_item(snap, i));
                            if has_assembled_candidate {
                                return Err(BridgeRpcError {
                                    code: "CannotSellAssembledItem".into(),
                                    message: format!("Item of type {type_id} in station {station_id} hangar must be repackaged before selling on the market"),
                                });
                            }
                            return Err(BridgeRpcError {
                                code: "InsufficientInventoryForSellOrder".into(),
                                message: format!("Seller does not have {quantity_i32}x repackaged consumable type {type_id} in station {station_id} hangar"),
                            });
                        };
                        fid
                    };

                    // Validate and consume exact itemID from seller's hangar at station_id
                    let Some(item) = snap.items.get(&item_id).cloned() else {
                        return Err(BridgeRpcError {
                            code: "InsufficientInventoryForSellOrder".into(),
                            message: format!("Item {item_id} not found in seller inventory"),
                        });
                    };
                    if is_courier_item(item_id) {
                        return Err(BridgeRpcError {
                            code: "ItemCannotBeSold".into(),
                            message: "Courier mission items cannot be sold on the market".into(),
                        });
                    }
                    if item.type_id != type_id || !is_sellable_item(snap, &item) {
                        return Err(BridgeRpcError {
                            code: "InsufficientInventoryForSellOrder".into(),
                            message: format!(
                                "Item {item_id} (type {type_id}) is not reachable in station {station_id} hangar"
                            ),
                        });
                    }
                    if item.singleton {
                        return Err(BridgeRpcError {
                            code: "CannotSellAssembledItem".into(),
                            message: format!(
                                "Item {item_id} of type {type_id} must be repackaged before selling"
                            ),
                        });
                    }
                    if snap.items.values().any(|other| {
                        other.item_id != item_id
                            && other.location_id == item_id
                            && other.quantity > 0
                    }) {
                        return Err(BridgeRpcError {
                            code: "CannotSellItemWithContents".into(),
                            message: format!(
                                "Cannot sell container/ship {item_id} while it contains items"
                            ),
                        });
                    }
                    if item.quantity < quantity_i32 {
                        return Err(BridgeRpcError {
                            code: "InsufficientInventoryForSellOrder".into(),
                            message: format!(
                                "Item {item_id} has quantity {}, less than requested {quantity_i32}",
                                item.quantity
                            ),
                        });
                    }
                    if let Some(item_mut) = snap.items.get_mut(&item_id) {
                        if item_mut.quantity > quantity_i32 {
                            item_mut.quantity -= quantity_i32;
                            item_mut.version += 1;
                        } else {
                            snap.items.remove(&item_id);
                        }
                    }

                    // Match against resting bid orders in region whose range reaches station_id
                    let mut candidate_ids: Vec<(i64, i64)> = snap
                        .market_orders
                        .values()
                        .filter(|o| {
                            o.type_id == type_id
                                && o.region_id == region_id
                                && o.is_bid
                                && o.vol_remaining > 0
                                && o.price.0 >= order_price.0
                                && can_bid_match_seller_station(ctx, snap, o, station_id, now_unix)
                        })
                        .map(|o| (o.order_id, o.price.0))
                        .collect();
                    candidate_ids.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

                    let mut remaining_qty = quantity_i32;
                    let mut first_matched_oid = None;

                    for (oid, _) in candidate_ids {
                        if remaining_qty <= 0 {
                            break;
                        }
                        let Some(resting) = snap.market_orders.get(&oid).cloned() else {
                            continue;
                        };
                        let required_min = resting.min_volume.max(1).min(resting.vol_remaining);
                        if remaining_qty < required_min {
                            continue;
                        }
                        let fill_qty = remaining_qty.min(resting.vol_remaining);
                        if fill_qty <= 0 {
                            continue;
                        }
                        let Some(fill_gross) = resting.price.checked_mul_qty(fill_qty as i64)
                        else {
                            continue;
                        };
                        let from_escrow = resting.escrow.min(fill_gross);
                        let uncovered = fill_gross.checked_sub(from_escrow).unwrap_or(IskCents(0));
                        if !resting.is_npc && uncovered.0 > 0 {
                            // Margin Trading order: cover shortfall from buyer's wallet or cancel order if insolvent
                            let covered = if let Some(buyer_ch) =
                                snap.characters.get_mut(&resting.owner_id)
                            {
                                if let Some(new_bal) = buyer_ch.balance.checked_sub(uncovered) {
                                    buyer_ch.balance = new_bal;
                                    snap.append_journal(
                                        resting.owner_id,
                                        "MarketEscrowCover",
                                        uncovered,
                                        new_bal,
                                        format!("Covered margin escrow shortfall for buy order {oid}"),
                                    );
                                    true
                                } else {
                                    false
                                }
                            } else if let Some(buyer_corp) =
                                snap.corporations.get_mut(&resting.owner_id)
                            {
                                if let Some(new_bal) =
                                    buyer_corp.master_wallet.checked_sub(uncovered)
                                {
                                    buyer_corp.master_wallet = new_bal;
                                    snap.append_journal(
                                        resting.owner_id,
                                        "MarketEscrowCover",
                                        uncovered,
                                        new_bal,
                                        format!("Covered margin escrow shortfall for buy order {oid}"),
                                    );
                                    true
                                } else {
                                    false
                                }
                            } else {
                                false
                            };
                            if !covered {
                                if resting.escrow.0 > 0 {
                                    if let Some(buyer_ch) =
                                        snap.characters.get_mut(&resting.owner_id)
                                    {
                                        buyer_ch.balance = buyer_ch
                                            .balance
                                            .checked_add(resting.escrow)
                                            .unwrap_or(buyer_ch.balance);
                                    }
                                }
                                if let Some(o_mut) = snap.market_orders.get_mut(&oid) {
                                    o_mut.vol_remaining = 0;
                                    o_mut.escrow = IskCents(0);
                                }
                                continue;
                            }
                        }

                        let fill_tax = compute_sales_tax_cents(fill_gross, acct_lvl);
                        total_sales_tax = total_sales_tax
                            .checked_add(fill_tax)
                            .unwrap_or(total_sales_tax);
                        let seller_net =
                            fill_gross.checked_sub(fill_tax).unwrap_or(IskCents(0));
                        let (seller_id, seller_bal) = if use_corp {
                            let bal = snap.corporations.get_mut(&caller_corp_id).map(|corp_mut| {
                                corp_mut.master_wallet =
                                    corp_mut.master_wallet.checked_add(seller_net).unwrap_or(corp_mut.master_wallet);
                                if let Some(d) = corp_mut.wallet_divisions.iter_mut().find(|d| d.division_id == div_id) {
                                    d.balance = d.balance.checked_add(seller_net).unwrap_or(d.balance);
                                }
                                corp_mut.master_wallet
                            });
                            (caller_corp_id, bal)
                        } else {
                            let bal = snap.characters.get_mut(&char_id).map(|seller| {
                                seller.balance = seller.balance.checked_add(seller_net).unwrap_or(seller.balance);
                                seller.balance
                            });
                            (char_id, bal)
                        };
                        // The buyer's ISK left their wallet as escrow when the bid was placed.
                        snap.journal_market_fill(eve_db::ledger::MarketFill {
                            buyer_id: resting.owner_id,
                            seller_id,
                            type_id,
                            quantity: fill_qty as i64,
                            price: resting.price,
                            location_id: station_id,
                            date_unix: now_unix,
                            gross: fill_gross,
                            buyer_balance_after: None,
                            seller_net,
                            seller_balance_after: seller_bal,
                            sales_tax: fill_tax,
                            tax_collector_id: tax_collector,
                            region_id: Some(resting.region_id),
                        });
                        if !resting.is_npc {
                            matched_buyers.push((oid, resting.owner_id));
                            let buyer_is_corp = snap.corporations.contains_key(&resting.owner_id);
                            snap.next_entity_id += 1;
                            let new_item_id = snap.next_entity_id;
                            snap.items.insert(
                                new_item_id,
                                InventoryItemRecord {
                                    item_id: new_item_id,
                                    type_id,
                                    owner_id: resting.owner_id,
                                    location_id: station_id,
                                    flag: if buyer_is_corp {
                                        InventoryFlag::Deliveries
                                    } else {
                                        InventoryFlag::Hangar
                                    },
                                    quantity: fill_qty,
                                    singleton: false,
                                    version: 1,
                                    ..Default::default()
                                },
                            );
                        }
                        let mut resting_completed = false;
                        if let Some(o_mut) = snap.market_orders.get_mut(&oid) {
                            o_mut.vol_remaining -= fill_qty;
                            o_mut.escrow =
                                o_mut.escrow.checked_sub(from_escrow).unwrap_or(IskCents(0));
                            if o_mut.vol_remaining == 0 && !o_mut.is_npc {
                                resting_completed = true;
                            }
                        }
                        if resting_completed {
                            let mut r_meta = load_market_order_meta(snap, &resting, now_unix);
                            r_meta.order_state = Some(2);
                            r_meta.final_vol_remaining = Some(0);
                            r_meta.closed_unix = Some(now_unix);
                            save_market_order_meta(snap, oid, r_meta);
                        }
                        remaining_qty -= fill_qty;
                        if first_matched_oid.is_none() {
                            first_matched_oid = Some(oid);
                        }
                    }

                    if remaining_qty == 0 {
                        // Immediate fill against resting bid orders: no resting order is placed and no broker fee is charged
                        order_ids.push(PyRep::Integer(first_matched_oid.unwrap_or(0)));
                        continue;
                    }
                    all_filled_immediately = false;

                    if item_dur == 0 {
                        return Err(BridgeRpcError {
                            code: "ImmediateOrderUnfilled".into(),
                            message: "Immediate sell order (duration=0) could not be completely filled".into(),
                        });
                    }

                    // Broker fee applies only to the resting portion (`remaining_qty`) with a 100.00 ISK minimum floor
                    let resting_cost = order_price
                        .checked_mul_qty(remaining_qty as i64)
                        .ok_or_else(|| BridgeRpcError {
                            code: "MarketOrderOverflow".into(),
                            message: "Order resting cost overflows 64-bit ISK cents".into(),
                        })?;
                    let raw_broker_fee = IskCents(
                        (((resting_cost.0 as i128) * broker_ppm + 500_000) / 1_000_000) as i64,
                    );
                    let broker_fee = raw_broker_fee.max(IskCents(10_000));

                    if use_corp {
                        let corp_bal = snap
                            .corporations
                            .get(&caller_corp_id)
                            .and_then(|c| c.wallet_divisions.iter().find(|d| d.division_id == div_id).map(|d| d.balance.min(c.master_wallet)))
                            .unwrap_or(IskCents(0));
                        if corp_bal.checked_sub(broker_fee).is_none() {
                            return Err(BridgeRpcError {
                                code: "InsufficientFunds".into(),
                                message: "Not enough corporation ISK for market broker fee".into(),
                            });
                        }
                        if let Some(corp_mut) = snap.corporations.get_mut(&caller_corp_id) {
                            corp_mut.master_wallet = corp_mut
                                .master_wallet
                                .checked_sub(broker_fee)
                                .unwrap_or(IskCents(0));
                            if let Some(d) = corp_mut.wallet_divisions.iter_mut().find(|d| d.division_id == div_id) {
                                d.balance = d.balance.checked_sub(broker_fee).unwrap_or(IskCents(0));
                            }
                            let bal_after = corp_mut.master_wallet;
                            snap.append_journal(
                                caller_corp_id,
                                "MarketBrokerFee",
                                broker_fee,
                                bal_after,
                                format!("Broker fee for sell order on type {type_id}"),
                            );
                        }
                    } else {
                        let caller_balance = snap
                            .characters
                            .get(&char_id)
                            .map(|c| c.balance)
                            .unwrap_or(IskCents(0));
                        if caller_balance.checked_sub(broker_fee).is_none() {
                            return Err(BridgeRpcError {
                                code: "InsufficientFunds".into(),
                                message: "Not enough ISK for market broker fee".into(),
                            });
                        }
                        if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
                            ch_mut.balance = ch_mut
                                .balance
                                .checked_sub(broker_fee)
                                .unwrap_or(IskCents(0));
                            let bal_after = ch_mut.balance;
                            snap.append_journal_entry(eve_db::ledger::JournalEntryInput {
                                owner_id: char_id,
                                entry_type: "MarketBrokerFee",
                                amount: IskCents(-broker_fee.0),
                                balance_after: bal_after,
                                reason: format!("Broker fee for sell order on type {type_id}"),
                                meta: eve_db::ledger::JournalMeta::new(
                                    eve_db::ledger::ref_types::BROKER_FEE,
                                    char_id,
                                    owner_corp_id,
                                    now_unix,
                                ),
                            });
                        }
                    }
                    if is_structure && broker_fee.0 > 0 {
                        let owner_share = structure_owner_broker_share(snap, docked_station_id, broker_fee);
                        if owner_share.0 > 0 {
                            if let Some(str_corp) = snap.corporations.get_mut(&owner_corp_id) {
                                str_corp.master_wallet = str_corp
                                    .master_wallet
                                    .checked_add(owner_share)
                                    .unwrap_or(str_corp.master_wallet);
                            }
                        }
                    }
                    total_broker_fee = total_broker_fee
                        .checked_add(broker_fee)
                        .unwrap_or(total_broker_fee);

                    let active_open_orders = snap
                        .market_orders
                        .values()
                        .filter(|o| o.owner_id == order_owner_id && !o.is_npc && o.vol_remaining > 0)
                        .count();
                    if active_open_orders >= max_open_orders {
                        return Err(BridgeRpcError {
                            code: "MaxOpenOrdersReached".into(),
                            message: format!(
                                "Character has reached maximum open market orders ({active_open_orders}/{max_open_orders})"
                            ),
                        });
                    }

                    snap.next_entity_id += 1;
                    let new_oid = snap.next_entity_id;
                    snap.market_orders.insert(
                        new_oid,
                        MarketOrderRecord {
                            order_id: new_oid,
                            owner_id: order_owner_id,
                            station_id,
                            region_id,
                            type_id,
                            is_bid: false,
                            price: order_price,
                            vol_entered: quantity_i32,
                            vol_remaining: remaining_qty,
                            min_volume: 1,
                            is_npc: false,
                            escrow: IskCents(0),
                        },
                    );
                    save_market_order_meta(
                        snap,
                        new_oid,
                        MarketOrderMeta {
                            range: 32767,
                            duration: item_dur,
                            issue_unix: now_unix,
                            order_state: None,
                            account_key: if use_corp { Some(account_key) } else { None },
                            final_vol_remaining: None,
                            closed_unix: None,
                        },
                    );
                    order_ids.push(PyRep::Integer(new_oid));
                }

                if let Some(expected_fee) = expected_brokers_fee {
                    if total_broker_fee.0 > expected_fee.0.saturating_add(1) {
                        return Err(BridgeRpcError {
                            code: "BrokerFeeExceeded".into(),
                            message: format!(
                                "Actual broker fee {:.2} ISK exceeds expectedBrokersFee {:.2} ISK",
                                total_broker_fee.to_wire_f64(),
                                expected_fee.to_wire_f64()
                            ),
                        });
                    }
                }

                Ok((order_ids, total_broker_fee, total_sales_tax, all_filled_immediately, matched_buyers))
            });

            match res {
                Ok((order_ids, total_broker, total_tax, all_filled_immediately, matched_buyers)) => {
                    let first_oid = order_ids
                        .first()
                        .and_then(|o| o.as_i64())
                        .unwrap_or(0);
                    for (matched_oid, buyer_id) in matched_buyers {
                        ctx.notify(
                            NotificationTarget::Character(buyer_id),
                            "OnOwnOrdersChanged",
                            "charid",
                            vec![
                                PyRep::Integer(matched_oid),
                                PyRep::String("Change".into()),
                                PyRep::Bool(false),
                            ],
                        );
                    }
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnOwnOrdersChanged",
                        "charid",
                        vec![
                            PyRep::Integer(first_oid),
                            PyRep::String(if all_filled_immediately { "Change" } else { "Add" }.into()),
                            PyRep::Bool(false),
                        ],
                    );
                    Some(Ok(PyRep::keyval([
                        ("orderID", PyRep::Integer(first_oid)),
                        ("filledImmediately", PyRep::Bool(all_filled_immediately)),
                        ("brokerFeeISK", PyRep::Real(total_broker.to_wire_f64())),
                        ("salesTaxISK", PyRep::Real(total_tax.to_wire_f64())),
                        ("placedCount", PyRep::Integer(order_ids.len() as i64)),
                        ("orderIDs", PyRep::List(order_ids)),
                        ("totalBrokerFeeISK", PyRep::Real(total_broker.to_wire_f64())),
                        ("totalSalesTaxISK", PyRep::Real(total_tax.to_wire_f64())),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        "CancelCharOrder" => {
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
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(order) = snap.market_orders.get(&order_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "OrderNotFound".into(),
                        message: format!("Market order {order_id} not found"),
                    });
                };
                let caller_corp_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0);
                let is_corp_order = order.owner_id != char_id && order.owner_id == caller_corp_id;
                if order.owner_id != char_id && !is_corp_order {
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
                let mut meta = load_market_order_meta(snap, &order, now_unix);
                let remaining_qty = order.vol_remaining;
                if order.is_bid {
                    // Buy order: refund remaining escrow to character or corporation balance
                    if order.escrow.0 > 0 {
                        if let Some(buyer) = snap.characters.get_mut(&order.owner_id) {
                            buyer.balance = buyer
                                .balance
                                .checked_add(order.escrow)
                                .unwrap_or(buyer.balance);
                            let bal_after = buyer.balance;
                            snap.append_journal_entry(eve_db::ledger::JournalEntryInput {
                                owner_id: order.owner_id,
                                entry_type: "MarketOrderCancelRefund",
                                amount: order.escrow,
                                balance_after: bal_after,
                                reason: format!("Refunded escrow for cancelled buy order {order_id}"),
                                meta: eve_db::ledger::JournalMeta::new(
                                    eve_db::ledger::ref_types::MARKET_ESCROW,
                                    0,
                                    order.owner_id,
                                    now_unix,
                                )
                                .with_context(eve_db::ledger::JournalContext::MarketOrder, order_id),
                            });
                        } else if let Some(corp) = snap.corporations.get_mut(&order.owner_id) {
                            corp.master_wallet = corp.master_wallet.checked_add(order.escrow).unwrap_or(corp.master_wallet);
                            let div_id = (meta.account_key.unwrap_or(1000).saturating_sub(999)).clamp(1, 7) as u8;
                            corp.ensure_wallet_divisions();
                            if let Some(d) = corp.wallet_divisions.iter_mut().find(|d| d.division_id == div_id) {
                                d.balance = d.balance.checked_add(order.escrow).unwrap_or(d.balance);
                            }
                            let bal_after = corp.master_wallet;
                            snap.append_journal(
                                order.owner_id,
                                "MarketOrderCancelRefund",
                                order.escrow,
                                bal_after,
                                format!("Refunded escrow for cancelled corporate buy order {order_id}"),
                            );
                        }
                    }
                } else {
                    // Sell order: return remaining items to seller's station hangar or corp deliveries (PLEX goes back to the PLEX Vault)
                    if order.type_id == crate::services::market_contracts::PLEX_TYPE_ID {
                        let _ = snap.adjust_plex_vault(char_id, remaining_qty as i64);
                    } else {
                        let target_flag = if is_corp_order { InventoryFlag::Deliveries } else { InventoryFlag::Hangar };
                        let existing_item = snap
                            .items
                            .values()
                            .find(|i| {
                                i.owner_id == order.owner_id
                                    && i.location_id == order.station_id
                                    && i.flag == target_flag
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
                                    owner_id: order.owner_id,
                                    location_id: order.station_id,
                                    flag: target_flag,
                                    quantity: remaining_qty,
                                    singleton: false,
                                    version: 1,
                                    ..Default::default()
                                },
                            );
                        }
                    }
                }

                if let Some(o_mut) = snap.market_orders.get_mut(&order_id) {
                    o_mut.vol_remaining = 0;
                    o_mut.escrow = IskCents(0);
                }
                meta.order_state = Some(3);
                meta.final_vol_remaining = Some(remaining_qty);
                meta.closed_unix = Some(now_unix);
                save_market_order_meta(snap, order_id, meta);
                Ok(())
            });
            match res {
                Ok(()) => {
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnOwnOrdersChanged",
                        "charid",
                        vec![
                            PyRep::Integer(order_id),
                            PyRep::String("Expiry".into()),
                            PyRep::Bool(false),
                        ],
                    );
                    Some(Ok(PyRep::Bool(true)))
                }
                Err(e) => Some(Err(e)),
            }
        }

        "BuyMultipleItems" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(station_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NotDockedAtStation".into(),
                    message: "Character must be docked at a station or structure to BuyMultipleItems".into(),
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

            let mut parsed_requests: Vec<(TypeId, i32, Option<IskCents>, i64, i64)> =
                Vec::with_capacity(raw_entries.len());
            for entry in raw_entries {
                let (tid_i64, qty_i64, max_price_f64, ent_st, ord_rng) = match entry {
                    PyRep::Tuple(els) | PyRep::List(els) => (
                        els.first().and_then(|e| e.as_i64()).unwrap_or(0),
                        els.get(1).and_then(|e| e.as_i64()).unwrap_or(0),
                        els.get(2).and_then(|e| e.as_f64()),
                        station_id,
                        -1,
                    ),
                    PyRep::KeyVal(kv) => (
                        kv.get("typeID").and_then(|e| e.as_i64()).unwrap_or(0),
                        kv.get("quantity").and_then(|e| e.as_i64()).unwrap_or(0),
                        kv.get("maxPrice").or_else(|| kv.get("price")).and_then(|e| e.as_f64()),
                        kv.get("stationID").and_then(|e| e.as_i64()).unwrap_or(station_id),
                        kv.get("orderRange").or_else(|| kv.get("range")).and_then(|e| e.as_i64()).unwrap_or(-1),
                    ),
                    PyRep::Dict(kv) => {
                        let find_key = |k: &str| kv.iter().find(|(ek, _)| ek.as_str() == Some(k)).map(|(_, ev)| ev);
                        (
                            find_key("typeID").and_then(|e| e.as_i64()).unwrap_or(0),
                            find_key("quantity").and_then(|e| e.as_i64()).unwrap_or(0),
                            find_key("maxPrice").or_else(|| find_key("price")).and_then(|e| e.as_f64()),
                            find_key("stationID").and_then(|e| e.as_i64()).unwrap_or(station_id),
                            find_key("orderRange").or_else(|| find_key("range")).and_then(|e| e.as_i64()).unwrap_or(-1),
                        )
                    }
                    _ => (0, 0, None, station_id, -1),
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
                parsed_requests.push((tid_u32, qty_i32, max_price_cents, ent_st, ord_rng));
            }

            let now_unix = ctx.now_unix();
            let region_id = session.region_id.unwrap_or(10_000_002);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut avail_isk = snap
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
                    .filter(|o| !o.is_bid && o.region_id == region_id && o.vol_remaining > 0)
                    .map(|o| (o.order_id, o.vol_remaining))
                    .collect();

                let mut planned_fills: Vec<(i64, TypeId, i32, IskCents, i64, bool, IskCents, i64)> = Vec::new();
                let mut item_statuses: Vec<PyRep> = Vec::with_capacity(parsed_requests.len());
                let mut total_cost = IskCents(0);
                let mut total_qty_bought: i64 = 0;
                let mut purchased_lines: i64 = 0;

                for (req_tid, req_qty, max_price_opt, ent_st, ord_rng) in &parsed_requests {
                    let mut candidate_orders: Vec<(i64, IskCents, i64, bool, i64)> = snap
                        .market_orders
                        .values()
                        .filter(|o| {
                            let in_range = if *ord_rng < 0 {
                                o.station_id == *ent_st
                            } else if *ord_rng >= 32767 {
                                o.region_id == region_id
                            } else {
                                let s1 = super::resolve_station_solar_system(ctx, snap, *ent_st);
                                let s2 = super::resolve_station_solar_system(ctx, snap, o.station_id);
                                match (s1, s2) {
                                    (Some(a), Some(b)) if a == b => true,
                                    (Some(a), Some(b)) if *ord_rng > 0 => super::solar_system_jump_distance(ctx, a, b).is_some_and(|d| (d as i64) <= *ord_rng),
                                    _ => false,
                                }
                            };
                            !o.is_bid
                                && in_range
                                && o.type_id == *req_tid
                                && virtual_rem.get(&o.order_id).copied().unwrap_or(0) > 0
                                && max_price_opt.map(|mp| o.price.0 <= mp.0).unwrap_or(true)
                        })
                        .map(|o| (o.order_id, o.price, o.owner_id, o.is_npc, o.station_id))
                        .collect();
                    candidate_orders
                        .sort_by(|a, b| a.1.0.cmp(&b.1.0).then_with(|| a.0.cmp(&b.0)));

                    let mut rem = *req_qty;
                    let mut filled_for_type: i32 = 0;
                    let mut stopped_by_isk = false;
                    for (oid, ask_price, seller_id, is_npc, ord_st_id) in candidate_orders {
                        if rem <= 0 {
                            break;
                        }
                        let avail = virtual_rem.get(&oid).copied().unwrap_or(0);
                        if avail <= 0 {
                            continue;
                        }
                        if ask_price.0 <= 0 {
                            continue;
                        }
                        let max_affordable = (avail_isk.0 / ask_price.0).min(avail as i64) as i32;
                        if max_affordable <= 0 {
                            stopped_by_isk = true;
                            break;
                        }
                        let desired = rem.min(avail);
                        let fill_qty = desired.min(max_affordable);
                        if fill_qty < desired {
                            stopped_by_isk = true;
                        }
                        let fill_gross = ask_price
                            .checked_mul_qty(fill_qty as i64)
                            .ok_or_else(|| BridgeRpcError {
                                code: "MarketOrderOverflow".into(),
                                message: "Order fill cost overflowed ISK bounds".into(),
                            })?;
                        avail_isk = avail_isk.checked_sub(fill_gross).unwrap_or(IskCents(0));
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
                        filled_for_type += fill_qty;
                        total_qty_bought += fill_qty as i64;
                        planned_fills.push((oid, *req_tid, fill_qty, fill_gross, seller_id, is_npc, ask_price, ord_st_id));
                    }

                    if filled_for_type > 0 {
                        purchased_lines += 1;
                    }
                    let status_str = if filled_for_type == *req_qty {
                        "Filled"
                    } else if filled_for_type > 0 {
                        "Partial"
                    } else if stopped_by_isk {
                        "InsufficientISK"
                    } else {
                        "Unfilled"
                    };
                    item_statuses.push(PyRep::keyval([
                        ("typeID", PyRep::Integer(*req_tid as i64)),
                        ("requestedQuantity", PyRep::Integer(*req_qty as i64)),
                        ("filledQuantity", PyRep::Integer(filled_for_type as i64)),
                        ("unfilledQuantity", PyRep::Integer(rem as i64)),
                        ("status", PyRep::String(status_str.into())),
                    ]));
                }

                let mut total_sales_tax = IskCents(0);
                let mut orders_created = Vec::with_capacity(planned_fills.len());

                for (oid, fill_tid, fill_qty, fill_gross, seller_id, is_npc, ask_price, ord_st_id) in planned_fills {
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
                        total_sales_tax = total_sales_tax
                            .checked_add(fill_tax)
                            .unwrap_or(total_sales_tax);
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
                            if fill_tax.0 > 0 {
                                snap.append_journal(
                                    seller_id,
                                    "TransactionTax",
                                    fill_tax,
                                    bal_after,
                                    format!("Sales tax for selling {fill_qty}x type {fill_tid}"),
                                );
                            }
                        }
                    }
                    let mut resting_completed = false;
                    let resting_clone = snap.market_orders.get(&oid).cloned();
                    if let Some(o_mut) = snap.market_orders.get_mut(&oid) {
                        o_mut.vol_remaining -= fill_qty;
                        if o_mut.vol_remaining == 0 && !o_mut.is_npc {
                            resting_completed = true;
                        }
                    }
                    if resting_completed {
                        if let Some(rc) = resting_clone {
                            let mut r_meta = load_market_order_meta(snap, &rc, now_unix);
                            r_meta.order_state = Some(2);
                            r_meta.final_vol_remaining = Some(0);
                            r_meta.closed_unix = Some(now_unix);
                            save_market_order_meta(snap, oid, r_meta);
                        }
                    }
                    snap.next_entity_id += 1;
                    let new_item_id = snap.next_entity_id;
                    snap.items.insert(
                        new_item_id,
                        InventoryItemRecord {
                            item_id: new_item_id,
                            type_id: fill_tid,
                            owner_id: char_id,
                            location_id: ord_st_id,
                            flag: InventoryFlag::Hangar,
                            quantity: fill_qty,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        },
                    );
                    orders_created.push(PyRep::keyval([
                        ("orderID", PyRep::Integer(oid)),
                        ("typeID", PyRep::Integer(fill_tid as i64)),
                        ("quantity", PyRep::Integer(fill_qty as i64)),
                        ("price", PyRep::Real(ask_price.to_wire_f64())),
                        ("itemID", PyRep::Integer(new_item_id)),
                    ]));
                }

                Ok((purchased_lines, total_qty_bought, total_cost, total_sales_tax, orders_created, item_statuses))
            });

            match res {
                Ok((line_count, total_qty, total_cost, total_sales_tax, orders_created, item_statuses)) => Some(Ok(PyRep::keyval([
                    ("ordersCreated", PyRep::List(orders_created)),
                    ("itemStatuses", PyRep::List(item_statuses)),
                    ("purchasedCount", PyRep::Integer(line_count)),
                    ("totalQuantity", PyRep::Integer(total_qty)),
                    ("totalCostISK", PyRep::Real(total_cost.to_wire_f64())),
                    ("salesTaxISK", PyRep::Real(total_sales_tax.to_wire_f64())),
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
            let raw_type_id = args.first().and_then(|a| {
                a.as_i64().or_else(|| a.dict_get("typeID").and_then(|v| v.as_i64()))
            }).or_else(|| _kwargs.get("typeID").and_then(|v| v.as_i64()));
            let Some(raw_type_id) = raw_type_id else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMarketOrder".into(),
                    message: "GetPriceHistory requires typeID".into(),
                }));
            };
            let type_id = raw_type_id as TypeId;

            let rows = ctx.db.with_snapshot(|snap| {
                crate::services::market_price_history::build_price_history_rows(snap, type_id, region_id)
            });
            let hdr = DBRowDescriptor::market_history_header();
            Some(Ok(hdr.build_crowset(rows)))
        }

        _ => None,
    }
}
