//! `market_orders_escrow` — Multi-Sell Orders, Order Cancellation, Multi-Buy Execution, & Price History (`E-1`, `E-2`).

#![forbid(unsafe_code)]

use super::{
    can_bid_match_seller_station, compute_sales_tax_cents, load_market_order_meta,
    save_market_order_meta, MarketOrderMeta,
};
use crate::formulas::compute_broker_fee_rate;
use crate::router::{NotificationTarget, RpcArgs, ServerContext};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

#[path = "market_orders_corp_auth.rs"]
pub(crate) mod market_orders_corp_auth;

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
            let rpc = RpcArgs::new(args, _kwargs);
            let use_corp = match rpc.opt_bool(1, "useCorp") {
                Ok(v) => v.unwrap_or(false),
                Err(e) => return Some(Err(e)),
            };
            let duration_days = match rpc.opt_i64(2, "duration") {
                Ok(v) => v.unwrap_or(90),
                Err(e) => return Some(Err(e)),
            };
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
                    Some(f) if f.is_finite() && f > 0.0 => Some(f),
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
                (sys.region_id == region_id).then(|| sys.stations.iter().find(|st| st.station_id == docked_station_id)).flatten()
            });
            let (station_owner_corp, owner_faction_id, is_structure) = if let Some(station_def) = station_info {
                (station_def.owner_corporation_id, station_def.owner_faction_id, false)
            } else {
                (0, None, true)
            };

            let mut parsed_items: Vec<(i64, TypeId, i64, IskCents, i32, i64)> = Vec::with_capacity(entries.len());
            for entry in entries {
                let (st_id, raw_tid, item_id, price_isk, qty, item_dur) = match entry {
                    PyRep::KeyVal(kv) => (
                        kv.get("stationID").and_then(|v| v.as_i64()).unwrap_or(docked_station_id),
                        kv.get("typeID").and_then(|v| v.as_i64()).unwrap_or(0),
                        kv.get("itemID").and_then(|v| v.as_i64()).unwrap_or(0),
                        kv.get("price").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        kv.get("quantity").and_then(|v| v.as_i64()).unwrap_or(0),
                        kv.get("duration").and_then(|v| v.as_i64()).unwrap_or(duration_days),
                    ),
                    PyRep::Dict(kv) => {
                        let find_key = |k: &str| kv.iter().find(|(ek, _)| ek.as_str() == Some(k)).map(|(_, ev)| ev);
                        (
                            find_key("stationID").and_then(|e| e.as_i64()).unwrap_or(docked_station_id),
                            find_key("typeID").and_then(|e| e.as_i64()).unwrap_or(0),
                            find_key("itemID").and_then(|e| e.as_i64()).unwrap_or(0),
                            find_key("price").and_then(|e| e.as_f64()).unwrap_or(0.0),
                            find_key("quantity").and_then(|e| e.as_i64()).unwrap_or(0),
                            find_key("duration").and_then(|e| e.as_i64()).unwrap_or(duration_days),
                        )
                    }
                    _ => return Some(Err(BridgeRpcError {
                        code: "InvalidMultiSellOrder".into(),
                        message: "Each PlaceMultiSellOrder entry must be a KeyVal/dict with stationID, typeID, itemID, price, quantity".into(),
                    })),
                };
                if st_id != docked_station_id {
                    return Some(Err(BridgeRpcError { code: "StationMismatch".into(), message: format!("Cannot place sell order at station {st_id} while docked at {docked_station_id}") }));
                }
                if raw_tid <= 0 || !price_isk.is_finite() || price_isk <= 0.0 || qty <= 0 || qty > (i32::MAX as i64) || ![0, 1, 3, 7, 14, 30, 90].contains(&item_dur) {
                    return Some(Err(BridgeRpcError { code: "InvalidMultiSellOrder".into(), message: "PlaceMultiSellOrder entry requires valid positive typeID, price, quantity, and duration".into() }));
                }
                if !super::super::market_contracts::is_valid_4_sig_fig_price(price_isk) {
                    return Some(Err(BridgeRpcError { code: "InvalidPriceTickSize".into(), message: format!("Sell order price {price_isk} violates EVE 4-significant-figure tick size rule") }));
                }
                let tid = raw_tid as TypeId;
                if ctx.sde.types.get(&tid).and_then(|t| t.market_group_id).is_none() {
                    return Some(Err(BridgeRpcError { code: "ItemNotMarketEligible".into(), message: format!("Type {tid} has no marketGroupID and cannot be sold on the market") }));
                }
                parsed_items.push((st_id, tid, item_id, IskCents::from_f64_isk(price_isk), qty as i32, item_dur));
            }

            let now_unix = ctx.now_unix();
            let tax_collector = crate::services::wallet_serverops::tax_collector_id(ctx);
            let damaged_fitted_ships: std::collections::HashSet<i64> = ctx
                .ship_fittings
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter(|(_, fit)| {
                    fit.rack_heat.iter().any(|&h| h > 0.0)
                        || fit.modules.values().any(|m| m.damage_hp > 0.0)
                })
                .map(|(&ship_id, _)| ship_id)
                .collect();
            // Execute the entire multi-sell batch atomically inside a single snapshot transaction
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let upwell_opt = crate::services::structure_profile_services::apply_due_profiles_and_check_upwell_market(
                    ctx, snap, docked_station_id, char_id, now_unix, "MarketHubOffline",
                )?;
                let (owner_corp_id, pilot_bps_opt) = if !is_structure {
                    (station_owner_corp, None)
                } else if let Some((st_corp_id, _, pilot_bps)) = upwell_opt {
                    if !snap.structures.get(&docked_station_id).and_then(|st| ctx.sde.solar_systems.get(&st.solar_system_id)).is_some_and(|sys| sys.region_id == region_id) {
                        return Err(BridgeRpcError { code: "InvalidMarketStation".into(), message: format!("Structure {docked_station_id} is not in region {region_id}") });
                    }
                    (st_corp_id, pilot_bps)
                } else {
                    return Err(BridgeRpcError { code: "InvalidMarketStation".into(), message: format!("Station {docked_station_id} is not in region {region_id}") });
                };
                let caller_corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                let div_id = (account_key.saturating_sub(999)).clamp(1, 7) as u8;
                if use_corp {
                    market_orders_corp_auth::require_corp_market_order_role(
                        snap,
                        caller_corp_id,
                        char_id,
                        account_key,
                        now_unix,
                    )?;
                    if let Some(corp) = snap.corporations.get_mut(&caller_corp_id) {
                        corp.ensure_wallet_divisions();
                    }
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
                let broker_rate = if let Some((_, st_rate, _)) = upwell_opt {
                    st_rate
                } else {
                    let m_sys = ctx.sde.stations.get(&docked_station_id).map(|s| s.solar_system_id).unwrap_or(0);
                    let m_tier = snap.fw_runtime.ihub_tiers.get(&m_sys).copied().unwrap_or(0);
                    let tier_mult = 1.0 - (f64::from(m_tier) * 0.10).min(0.50);
                    compute_broker_fee_rate(broker_lvl, faction_standing, corp_standing) * tier_mult
                };
                let broker_ppm = (broker_rate * 1_000_000.0).round() as i128;

                let damaged_count = crate::services::notify_emitters_skills_trade_godma::count_damaged_sell_items(snap, &damaged_fitted_ships, char_id, caller_corp_id, use_corp, &parsed_items);
                if damaged_count > 0 {
                    return Err(BridgeRpcError { code: "CantRepackageDamagedItem".into(), message: damaged_count.to_string() });
                }

                let ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
                let mut order_ids = Vec::with_capacity(parsed_items.len());
                let mut total_broker_fee = IskCents(0);
                let mut total_sales_tax = IskCents(0);
                let mut all_filled_immediately = true;
                let mut matched_buyers: Vec<(i64, i64)> = Vec::new();
                let mut consumed_items: Vec<(InventoryItemRecord, Vec<(i32, PyRep)>)> = Vec::new();
                let mut buyer_acct_updates: Vec<(bool, i64, u8, IskCents)> = Vec::new();
                let mut delivered_items: Vec<(i64, InventoryItemRecord)> = Vec::new();
                let mut seller_acct_final: Option<(bool, i64, u8, IskCents)> = None;

                for (station_id, type_id, raw_item_id, order_price, quantity_i32, item_dur) in parsed_items {
                    let _ = order_price
                        .checked_mul_qty(quantity_i32 as i64)
                        .ok_or_else(|| BridgeRpcError {
                            code: "MarketOrderOverflow".into(),
                            message: "Order total cost overflows 64-bit ISK cents".into(),
                        })?;
                    let is_sellable_item = |snap_ref: &eve_db::WorldStateSnapshot, i: &InventoryItemRecord| {
                        market_orders_corp_auth::is_item_sellable_on_market(
                            snap_ref,
                            &ext,
                            i,
                            char_id,
                            caller_corp_id,
                            use_corp,
                            station_id,
                            session.ship_id,
                            now_unix,
                        )
                    };
                    let is_sellable_ignoring_contents = |snap_ref: &eve_db::WorldStateSnapshot, i: &InventoryItemRecord| {
                        market_orders_corp_auth::is_item_sellable_on_market_ignoring_contents(
                            snap_ref,
                            &ext,
                            i,
                            char_id,
                            caller_corp_id,
                            use_corp,
                            station_id,
                            session.ship_id,
                            now_unix,
                        )
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
                                    && is_sellable_ignoring_contents(snap, i)
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
                            if let Some(cand) = snap.items.values().find(|i| i.type_id == type_id && i.owner_id == char_id && snap.items.get(&i.location_id).is_some_and(|p| crate::services::inventory_fitting::fitting_containers_ships::is_container_group(ctx.sde.types.get(&p.type_id).map(|t| t.group_id).unwrap_or(0), p.type_id))) {
                                return Err(BridgeRpcError {
                                    code: "ItemInsideContainer".into(),
                                    message: format!("Item {} is inside a container and must be moved to item hangar before selling", cand.item_id),
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
                    if snap.items.get(&item.location_id).is_some_and(|p| crate::services::inventory_fitting::fitting_containers_ships::is_container_group(ctx.sde.types.get(&p.type_id).map(|t| t.group_id).unwrap_or(0), p.type_id)) {
                        return Err(BridgeRpcError {
                            code: "ItemInsideContainer".into(),
                            message: format!("Item {item_id} is inside a container and must be moved to item hangar before selling"),
                        });
                    }
                    if is_courier_item(item_id) {
                        return Err(BridgeRpcError {
                            code: "ItemCannotBeSold".into(),
                            message: "Courier mission items cannot be sold on the market".into(),
                        });
                    }
                    if item.type_id != type_id || !is_sellable_ignoring_contents(snap, &item) {
                        return Err(BridgeRpcError {
                            code: "InsufficientInventoryForSellOrder".into(),
                            message: format!(
                                "Item {item_id} (type {type_id}) is not reachable in station {station_id} hangar"
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
                    if item.singleton {
                        return Err(BridgeRpcError {
                            code: "CannotSellAssembledItem".into(),
                            message: format!(
                                "Item {item_id} of type {type_id} must be repackaged before selling"
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
                    let old_qty = item.quantity;
                    let old_loc = item.location_id;
                    let mut sell_opts = eve_db::ConsumeExactOptions::for_character_stack(char_id);
                    sell_opts.expected_owner_id = item.owner_id;
                    sell_opts.reject_non_empty_container = true;
                    sell_opts.active_ship_id = snap.characters.get(&char_id).map(|c| c.active_ship_id);
                    match snap.consume_exact_item_quantity(item_id, quantity_i32, sell_opts) {
                        Ok(Some(updated)) => {
                            consumed_items.push((
                                updated,
                                // V24.01 invCache uses ixStackSize (9), not legacy ixQuantity (5).
                                vec![(9, PyRep::Integer(old_qty as i64))],
                            ));
                        }
                        Ok(None) => {
                            let mut removed_rec = item.clone();
                            removed_rec.location_id = 6;
                            removed_rec.flag = InventoryFlag::None;
                            removed_rec.quantity = 0;
                            consumed_items.push((
                                removed_rec,
                                vec![(3, PyRep::Integer(old_loc)), (9, PyRep::Integer(old_qty as i64))],
                            ));
                        }
                        Err(e) => {
                            return Err(BridgeRpcError {
                                code: "ItemLocked".into(),
                                message: e.to_string(),
                            });
                        }
                    }

                    // Match against resting bid orders in region whose range reaches station_id
                    let mut candidate_ids: Vec<(i64, i64)> = snap
                        .market_orders
                        .values()
                        .filter(|o| {
                            let struct_ok = if o.station_id == station_id {
                                true
                            } else {
                                crate::services::market_trading::is_structure_market_online(snap, o.station_id)
                            };
                            o.type_id == type_id
                                && struct_ok
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
                            let r_meta = load_market_order_meta(snap, &resting, now_unix);
                            let r_div_id = r_meta
                                .account_key
                                .map(|ak| (ak.saturating_sub(999)).clamp(1, 7) as u8)
                                .unwrap_or(1);
                            let covered = if let Some(buyer_ch) =
                                snap.characters.get_mut(&resting.owner_id)
                            {
                                if let Some(new_bal) = buyer_ch.balance.checked_sub(uncovered) {
                                    buyer_ch.balance = new_bal;
                                    buyer_acct_updates.push((false, resting.owner_id, 1, new_bal));
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
                                let div_bal = buyer_corp.division_balance(r_div_id);
                                if let Some(new_bal) = div_bal.checked_sub(uncovered) {
                                    buyer_corp.set_division_balance(r_div_id, new_bal);
                                    buyer_acct_updates.push((true, resting.owner_id, r_div_id, new_bal));
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
                                        buyer_acct_updates.push((false, resting.owner_id, 1, buyer_ch.balance));
                                    } else if let Some(buyer_corp) =
                                        snap.corporations.get_mut(&resting.owner_id)
                                    {
                                        let rb = buyer_corp.credit_division(r_div_id, resting.escrow);
                                        buyer_acct_updates.push((true, resting.owner_id, r_div_id, rb));
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
                                corp_mut.credit_division(div_id, seller_net)
                            });
                            (caller_corp_id, bal)
                        } else {
                            let bal = snap.characters.get_mut(&char_id).map(|seller| {
                                seller.balance = seller.balance.checked_add(seller_net).unwrap_or(seller.balance);
                                seller.balance
                            });
                            (char_id, bal)
                        };
                        if let Some(bal) = seller_bal {
                            seller_acct_final = Some((use_corp, seller_id, div_id, bal));
                        }
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
                            let delivered_rec = InventoryItemRecord {
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
                            };
                            snap.items.insert(new_item_id, delivered_rec.clone());
                            let r_issued = snap.market_order_meta.get(&oid).and_then(|m| m.issued_by);
                            let rec_char = super::resolve_corp_recipient_char(snap, resting.owner_id, r_issued);
                            delivered_items.push((rec_char, delivered_rec));
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
                            .map(|c| c.division_balance(div_id))
                            .unwrap_or(IskCents(0));
                        let Some(bal_after) = corp_bal.checked_sub(broker_fee) else {
                            return Err(BridgeRpcError {
                                code: "InsufficientFunds".into(),
                                message: "Not enough corporation ISK for market broker fee".into(),
                            });
                        };
                        if let Some(corp_mut) = snap.corporations.get_mut(&caller_corp_id) {
                            corp_mut.set_division_balance(div_id, bal_after);
                            seller_acct_final = Some((true, caller_corp_id, div_id, bal_after));
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
                            seller_acct_final = Some((false, char_id, 1, bal_after));
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
                        let owner_share = super::structure_owner_broker_share_with_pilot(
                            snap,
                            docked_station_id,
                            broker_fee,
                            pilot_bps_opt,
                        );
                        if owner_share.0 > 0 {
                            crate::services::station_structure::structure_upwell_ops::credit_structure_owner_corp_div1(
                                snap,
                                owner_corp_id,
                                owner_share,
                                eve_db::ledger::ref_types::BROKER_FEE,
                                "MarketBrokerFee",
                                order_owner_id,
                                format!("Structure market broker fee for sell order on type {type_id}"),
                                now_unix,
                            );
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
                    if let Some(m) = snap.market_order_meta.get_mut(&new_oid) {
                        m.issued_by = Some(char_id);
                    }
                    order_ids.push(PyRep::Integer(new_oid));
                }

                if let Some(expected_fee_raw) = expected_brokers_fee {
                    if expected_fee_raw <= 1.0 {
                        if broker_rate > expected_fee_raw + 1e-6 {
                            return Err(BridgeRpcError {
                                code: "BrokerFeeExceeded".into(),
                                message: format!("Actual broker fee rate {broker_rate:.4} exceeds expectedBrokersFee {expected_fee_raw:.4}"),
                            });
                        }
                    } else {
                        let expected_fee = IskCents::from_f64_isk(expected_fee_raw);
                        if total_broker_fee.0 > expected_fee.0.saturating_add(1) {
                            return Err(BridgeRpcError {
                                code: "BrokerFeeExceeded".into(),
                                message: format!("Actual broker fee {:.2} ISK exceeds expectedBrokersFee {:.2} ISK", total_broker_fee.to_wire_f64(), expected_fee.to_wire_f64()),
                            });
                        }
                    }
                }

                Ok((order_ids, total_broker_fee, total_sales_tax, all_filled_immediately, matched_buyers, consumed_items, buyer_acct_updates, delivered_items, seller_acct_final))
            });

            match res {
                Ok((order_ids, total_broker, total_tax, all_filled_immediately, matched_buyers, consumed_items, buyer_acct_updates, delivered_items, seller_acct_final)) => {
                    let first_oid = order_ids
                        .first()
                        .and_then(|o| o.as_i64())
                        .unwrap_or(0);
                    for (rec, changes) in &consumed_items {
                        super::emit_market_item_change(ctx, char_id, rec, changes.clone());
                    }
                    for (rec_char, rec) in &delivered_items {
                        super::emit_market_item_change(ctx, *rec_char, rec, vec![(3, PyRep::Integer(0))]);
                    }
                    for (b_is_corp, b_owner, b_div, b_bal) in buyer_acct_updates {
                        super::emit_market_account_change(ctx, b_is_corp, b_owner, b_div, b_bal);
                    }
                    if let Some((s_is_corp, s_owner, s_div, s_bal)) = seller_acct_final {
                        super::emit_market_account_change(ctx, s_is_corp, s_owner, s_div, s_bal);
                    }
                    for (matched_oid, buyer_id) in matched_buyers {
                        ctx.notify(
                            NotificationTarget::Character(buyer_id),
                            "OnOwnOrdersChanged",
                            "charid",
                            crate::services::market_price_history::build_own_order_changed_payload(ctx, matched_oid, "Change", false),
                        );
                    }
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnOwnOrdersChanged",
                        "charid",
                        crate::services::market_price_history::build_own_order_changed_payload(
                            ctx,
                            first_oid,
                            if all_filled_immediately { "Change" } else { "Add" },
                            use_corp,
                        ),
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
                Err(e) => {
                    if e.code == "CantRepackageDamagedItem" {
                        let count = e.message.parse::<i64>().unwrap_or(1).max(1);
                        crate::services::notify_emitters_skills_trade_godma::emit_sell_repackage_errors(ctx, char_id, "CantRepackageDamagedItem", count);
                    }
                    Some(Err(e))
                }
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
                let mut meta = load_market_order_meta(snap, &order, now_unix);
                if is_corp_order {
                    if !snap.corporations.contains_key(&caller_corp_id) {
                        return Err(BridgeRpcError {
                            code: "CorporationNotFound".into(),
                            message: "Corporation not found".into(),
                        });
                    }
                    let issued_by = snap.market_order_meta.get(&order_id).and_then(|m| m.issued_by);
                    if issued_by != Some(char_id) {
                        let order_acct_key = meta.account_key.unwrap_or_else(|| {
                            snap.market_order_meta
                                .get(&order_id)
                                .and_then(|m| m.wallet_division)
                                .map(|d| (999 + d).clamp(1000, 1006) as u16)
                                .unwrap_or(1000)
                        });
                        market_orders_corp_auth::require_corp_market_order_role(
                            snap,
                            caller_corp_id,
                            char_id,
                            order_acct_key,
                            now_unix,
                        )?;
                    }
                }
                if order.vol_remaining <= 0 {
                    return Err(BridgeRpcError {
                        code: "OrderAlreadyFilled".into(),
                        message: format!("Order {order_id} is already filled or cancelled"),
                    });
                }
                let remaining_qty = order.vol_remaining;
                let mut refund_acct: Option<(bool, i64, u8, IskCents)> = None;
                let mut returned_item: Option<(i64, InventoryItemRecord, Vec<(i32, PyRep)>)> = None;
                if order.is_bid {
                    // Buy order: refund remaining escrow to character or corporation balance
                    if order.escrow.0 > 0 {
                        if let Some(buyer) = snap.characters.get_mut(&order.owner_id) {
                            buyer.balance = buyer
                                .balance
                                .checked_add(order.escrow)
                                .unwrap_or(buyer.balance);
                            let bal_after = buyer.balance;
                            refund_acct = Some((false, order.owner_id, 1, bal_after));
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
                            let div_id = (meta.account_key.unwrap_or(1000).saturating_sub(999)).clamp(1, 7) as u8;
                            let bal_after = corp.credit_division(div_id, order.escrow);
                            refund_acct = Some((true, order.owner_id, div_id, bal_after));
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
                    // Sell order: return remaining items to seller's station hangar or corp deliveries (personal PLEX goes back to the PLEX Vault)
                    if order.type_id == crate::services::market_contracts::PLEX_TYPE_ID
                        && !is_corp_order
                    {
                        let _ = snap.adjust_plex_vault(order.owner_id, remaining_qty as i64);
                    } else {
                        let issued_by = snap.market_order_meta.get(&order_id).and_then(|m| m.issued_by);
                        let recipient_char = super::resolve_corp_recipient_char(snap, order.owner_id, issued_by);
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
                                let old_qty = item.quantity;
                                item.quantity = item.quantity.saturating_add(remaining_qty);
                                item.version += 1;
                                returned_item = Some((
                                    recipient_char,
                                    item.clone(),
                                    // V24.01 invCache uses ixStackSize (9), not legacy ixQuantity (5).
                                    vec![(9, PyRep::Integer(old_qty as i64))],
                                ));
                            }
                        } else {
                            snap.next_entity_id += 1;
                            let nid = snap.next_entity_id;
                            let new_rec = InventoryItemRecord {
                                item_id: nid,
                                type_id: order.type_id,
                                owner_id: order.owner_id,
                                location_id: order.station_id,
                                flag: target_flag,
                                quantity: remaining_qty,
                                singleton: false,
                                version: 1,
                                ..Default::default()
                            };
                            snap.items.insert(nid, new_rec.clone());
                            returned_item = Some((
                                recipient_char,
                                new_rec,
                                vec![(3, PyRep::Integer(0))],
                            ));
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
                Ok((refund_acct, returned_item, is_corp_order))
            });
            match res {
                Ok((refund_acct, returned_item, is_corp_order)) => {
                    if let Some((r_is_corp, r_owner, r_div, r_bal)) = refund_acct {
                        super::emit_market_account_change(ctx, r_is_corp, r_owner, r_div, r_bal);
                    }
                    if let Some((rec_char, rec, changes)) = returned_item {
                        super::emit_market_item_change(ctx, rec_char, &rec, changes);
                    }
                    ctx.notify(NotificationTarget::Character(char_id), "OnOwnOrdersChanged", "charid", crate::services::market_price_history::build_own_order_changed_payload(ctx, order_id, "Expiry", is_corp_order));
                    Some(Ok(PyRep::Bool(true)))
                }
                Err(e) => Some(Err(e)),
            }
        }

        "BuyMultipleItems" => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let docked_station_id = session.station_id.or(session.structure_id);
            let (station_id, raw_entries) = match (args.first(), args.get(1)) {
                (Some(PyRep::List(entries) | PyRep::Tuple(entries)), _) => {
                    let Some(sid) = docked_station_id else {
                        return Some(Err(BridgeRpcError { code: "NotDockedAtStation".into(), message: "Character must be docked at a station or structure to BuyMultipleItems".into() }));
                    };
                    (sid, entries.as_slice())
                }
                (Some(PyRep::Integer(sid)), Some(PyRep::List(entries) | PyRep::Tuple(entries))) => {
                    if let Some(docked) = docked_station_id {
                        if *sid != docked {
                            return Some(Err(BridgeRpcError {
                                code: "StationMismatch".into(),
                                message: format!(
                                    "BuyMultipleItems station {sid} does not match docked station {docked}"
                                ),
                            }));
                        }
                    }
                    (*sid, entries.as_slice())
                }
                _ => {
                    if docked_station_id.is_none() && args.is_empty() {
                        return Some(Err(BridgeRpcError { code: "NotDockedAtStation".into(), message: "Character must be docked at a station or structure to BuyMultipleItems".into() }));
                    }
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
                        kv.get("quantity").or_else(|| kv.get("qty")).and_then(|e| e.as_i64()).unwrap_or(0),
                        kv.get("maxPrice").or_else(|| kv.get("price")).and_then(|e| e.as_f64()),
                        kv.get("stationID").and_then(|e| e.as_i64()).unwrap_or(station_id),
                        kv.get("orderRange").or_else(|| kv.get("range")).and_then(|e| e.as_i64()).unwrap_or(-1),
                    ),
                    PyRep::Dict(kv) => {
                        let find_key = |k: &str| kv.iter().find(|(ek, _)| ek.as_str() == Some(k)).map(|(_, ev)| ev);
                        (
                            find_key("typeID").and_then(|e| e.as_i64()).unwrap_or(0),
                            find_key("quantity").or_else(|| find_key("qty")).and_then(|e| e.as_i64()).unwrap_or(0),
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
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let _ = crate::services::structure_profile_services::apply_due_profiles_and_check_upwell_market(
                    ctx, snap, station_id, char_id, now_unix, "MarketServiceOffline",
                )?;
                let st_sys = super::resolve_station_solar_system(ctx, snap, station_id);
                let st_reg = st_sys.and_then(|s| ctx.sde.solar_systems.get(&s).map(|sys| sys.region_id));
                let region_id = match session.region_id {
                    Some(reg) => {
                        if st_reg != Some(reg) {
                            return Err(BridgeRpcError {
                                code: "InvalidMarketStation".into(),
                                message: format!("Station {station_id} is not in region {reg}"),
                            });
                        }
                        reg
                    }
                    None => st_reg.ok_or_else(|| BridgeRpcError {
                        code: "InvalidMarketStation".into(),
                        message: format!("Could not resolve region for station {station_id}"),
                    })?,
                };
                let sk = snap.skills.get(&char_id);
                let vis_lvl = sk.and_then(|m| m.get(&3447)).map(|s| s.level).unwrap_or(0);
                let max_vis_jumps = crate::services::market_contracts::max_remote_jumps_for_skill_level(vis_lvl);
                let proc_lvl = sk.and_then(|m| m.get(&16594)).map(|s| s.level).unwrap_or(0);
                let max_proc_jumps = crate::services::market_contracts::max_remote_jumps_for_skill_level(proc_lvl);
                let caller_sys = if docked_station_id.is_some() {
                    st_sys
                } else {
                    session.solar_system_id.or(st_sys)
                };
                for (req_tid, _, _, ent_st, ord_rng) in &parsed_requests {
                    if !matches!(*ord_rng, -1 | 0 | 1..=40 | 32767) {
                        return Err(BridgeRpcError { code: "InvalidBuyMultipleItems".into(), message: format!("Invalid orderRange {ord_rng} in BuyMultipleItems") });
                    }
                    if *ent_st != station_id {
                        let _ = crate::services::structure_profile_services::apply_due_profiles_and_check_upwell_market(
                            ctx, snap, *ent_st, char_id, now_unix, "MarketServiceOffline",
                        )?;
                        let ent_sys = super::resolve_station_solar_system(ctx, snap, *ent_st);
                        let ent_reg = ent_sys.and_then(|s| ctx.sde.solar_systems.get(&s).map(|sys| sys.region_id));
                        if ent_reg != Some(region_id) {
                            return Err(BridgeRpcError { code: "InvalidMarketStation".into(), message: format!("Station {ent_st} is not in region {region_id}") });
                        }
                        let proc_ok = proc_lvl > 0 && (max_proc_jumps >= 32_767 || match (caller_sys, ent_sys) {
                            (Some(a), Some(b)) if a == b => true,
                            (Some(a), Some(b)) if max_proc_jumps > 0 => super::solar_system_jump_distance(ctx, a, b).is_some_and(|d| (d as i64) <= max_proc_jumps),
                            _ => false,
                        });
                        if !proc_ok {
                            return Err(BridgeRpcError { code: "InsufficientProcurementSkill".into(), message: format!("Procurement skill level {proc_lvl} is insufficient for remote station {ent_st}") });
                        }
                    }
                    if *req_tid != super::PLEX_TYPE_ID && *ord_rng > max_vis_jumps {
                        return Err(BridgeRpcError { code: "InsufficientVisibilitySkill".into(), message: format!("Order range {ord_rng} exceeds Visibility skill limit {max_vis_jumps}") });
                    }
                }
                let mut avail_isk = snap.characters.get(&char_id).map(|c| c.balance).ok_or_else(|| BridgeRpcError { code: "CharacterNotFound".into(), message: format!("Character {char_id} not found") })?;
                let mut virtual_rem: HashMap<i64, i32> = snap.market_orders.values().filter(|o| !o.is_bid && o.region_id == region_id && o.vol_remaining > 0).map(|o| (o.order_id, o.vol_remaining)).collect();

                let mut planned_fills: Vec<(i64, TypeId, i32, IskCents, i64, bool, IskCents, i64, u32)> = Vec::new();
                let mut item_statuses: Vec<PyRep> = Vec::with_capacity(parsed_requests.len());
                let mut total_cost = IskCents(0);
                let mut total_qty_bought: i64 = 0;
                let mut purchased_lines: i64 = 0;

                for (req_tid, req_qty, max_price_opt, ent_st, ord_rng) in &parsed_requests {
                    let mut candidate_orders: Vec<(i64, IskCents, i64, bool, i64, u32)> = snap
                        .market_orders
                        .values()
                        .filter(|o| {
                            let in_range = o.region_id == region_id && if *ord_rng < 0 {
                                o.station_id == *ent_st
                            } else if *ord_rng >= 32767 {
                                true
                            } else {
                                let s1 = super::resolve_station_solar_system(ctx, snap, *ent_st);
                                let s2 = super::resolve_station_solar_system(ctx, snap, o.station_id);
                                match (s1, s2) {
                                    (Some(a), Some(b)) if a == b => true,
                                    (Some(a), Some(b)) if *ord_rng > 0 => super::solar_system_jump_distance(ctx, a, b).is_some_and(|d| (d as i64) <= *ord_rng),
                                    _ => false,
                                }
                            };
                            let struct_ok = o.station_id == *ent_st
                                || !snap.structures.contains_key(&o.station_id)
                                || crate::services::structure_profile_services::check_upwell_market_access_and_rate(
                                    ctx, snap, o.station_id, char_id, now_unix, "MarketServiceOffline",
                                ).is_ok();
                            !o.is_bid
                                && struct_ok
                                && in_range
                                && o.type_id == *req_tid
                                && virtual_rem.get(&o.order_id).copied().unwrap_or(0) > 0
                                && max_price_opt.map(|mp| o.price.0 <= mp.0).unwrap_or(true)
                        })
                        .map(|o| (o.order_id, o.price, o.owner_id, o.is_npc, o.station_id, o.region_id))
                        .collect();
                    candidate_orders
                        .sort_by(|a, b| a.1.0.cmp(&b.1.0).then_with(|| a.0.cmp(&b.0)));

                    let mut rem = *req_qty;
                    let mut filled_for_type: i32 = 0;
                    let mut stopped_by_isk = false;
                    for (oid, ask_price, seller_id, is_npc, ord_st_id, ord_reg_id) in candidate_orders {
                        if rem <= 0 { break; }
                        let avail = virtual_rem.get(&oid).copied().unwrap_or(0);
                        if avail <= 0 || ask_price.0 <= 0 { continue; }
                        let max_affordable = (avail_isk.0 / ask_price.0).min(avail as i64) as i32;
                        if max_affordable <= 0 { stopped_by_isk = true; break; }
                        let desired = rem.min(avail);
                        let fill_qty = desired.min(max_affordable);
                        if fill_qty < desired { stopped_by_isk = true; }
                        let fill_gross = ask_price.checked_mul_qty(fill_qty as i64).ok_or_else(|| BridgeRpcError {
                            code: "MarketOrderOverflow".into(),
                            message: "Order fill cost overflowed ISK bounds".into(),
                        })?;
                        avail_isk = avail_isk.checked_sub(fill_gross).unwrap_or(IskCents(0));
                        total_cost = total_cost.checked_add(fill_gross).ok_or_else(|| BridgeRpcError {
                            code: "MarketOrderOverflow".into(),
                            message: "Total BuyMultipleItems cost overflowed ISK bounds".into(),
                        })?;
                        if let Some(vr) = virtual_rem.get_mut(&oid) { *vr -= fill_qty; }
                        rem -= fill_qty;
                        filled_for_type += fill_qty;
                        total_qty_bought += fill_qty as i64;
                        planned_fills.push((oid, *req_tid, fill_qty, fill_gross, seller_id, is_npc, ask_price, ord_st_id, ord_reg_id));
                    }

                    if filled_for_type > 0 { purchased_lines += 1; }
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
                let tax_collector = crate::services::wallet_serverops::tax_collector_id(ctx);
                let mut buyer_bal_final: Option<IskCents> = None;
                let mut seller_acct_updates: Vec<(bool, i64, u8, IskCents)> = Vec::new();
                let mut delivered_item_records: Vec<InventoryItemRecord> = Vec::new();

                for (oid, fill_tid, fill_qty, fill_gross, seller_id, is_npc, ask_price, ord_st_id, ord_reg_id) in planned_fills {
                    let buyer_bal = snap.characters.get_mut(&char_id).map(|buyer| {
                        buyer.balance = buyer.balance.checked_sub(fill_gross).unwrap_or(IskCents(0));
                        buyer.balance
                    });
                    if let Some(b) = buyer_bal {
                        buyer_bal_final = Some(b);
                    }
                    let (mut seller_net, mut seller_bal, mut fill_tax) = (IskCents(0), None, IskCents(0));
                    if !is_npc {
                        let seller_meta = snap.market_orders.get(&oid).map(|o| load_market_order_meta(snap, o, now_unix));
                        let seller_acct_lvl = snap.skills.get(&seller_id).and_then(|m| m.get(&16622)).map(|s| s.level).unwrap_or(0);
                        fill_tax = compute_sales_tax_cents(fill_gross, seller_acct_lvl);
                        total_sales_tax = total_sales_tax.checked_add(fill_tax).unwrap_or(total_sales_tax);
                        seller_net = fill_gross.checked_sub(fill_tax).unwrap_or(IskCents(0));
                        if let Some(seller) = snap.characters.get_mut(&seller_id) {
                            seller.balance = seller.balance.checked_add(seller_net).unwrap_or(seller.balance);
                            seller_bal = Some(seller.balance);
                            seller_acct_updates.push((false, seller_id, 1, seller.balance));
                        } else if let Some(seller_corp) = snap.corporations.get_mut(&seller_id) {
                            let s_div = seller_meta.and_then(|m| m.account_key).map(|k| (k.saturating_sub(999)).clamp(1, 7) as u8).unwrap_or(1);
                            let sb = seller_corp.credit_division(s_div, seller_net);
                            seller_bal = Some(sb);
                            seller_acct_updates.push((true, seller_id, s_div, sb));
                        }
                    }

                    snap.journal_market_fill(eve_db::ledger::MarketFill {
                        buyer_id: char_id, seller_id, type_id: fill_tid, quantity: fill_qty as i64, price: ask_price,
                        location_id: ord_st_id, date_unix: now_unix, gross: fill_gross, buyer_balance_after: buyer_bal,
                        seller_net, seller_balance_after: seller_bal, sales_tax: fill_tax, tax_collector_id: tax_collector,
                        region_id: Some(ord_reg_id),
                    });

                    let mut resting_completed = false;
                    let resting_clone = snap.market_orders.get(&oid).cloned();
                    let is_npc_skillbook_or_bpo = is_npc
                        && seller_id < 90_000_000
                        && ctx
                            .sde
                            .types
                            .get(&fill_tid)
                            .is_some_and(|t| matches!(t.category_id, 9 | 16));
                    if let Some(o_mut) = snap.market_orders.get_mut(&oid) {
                        if is_npc_skillbook_or_bpo {
                            o_mut.vol_remaining = o_mut.vol_entered.max(1);
                        } else {
                            o_mut.vol_remaining -= fill_qty;
                            if o_mut.vol_remaining == 0 && !o_mut.is_npc {
                                resting_completed = true;
                            }
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

                    let item_id = if fill_tid == crate::services::market_contracts::PLEX_TYPE_ID {
                        let _ = snap.adjust_plex_vault(char_id, fill_qty as i64);
                        0
                    } else {
                        snap.next_entity_id += 1;
                        let new_item_id = snap.next_entity_id;
                        let rec = InventoryItemRecord {
                            item_id: new_item_id, type_id: fill_tid, owner_id: char_id, location_id: ord_st_id,
                            flag: InventoryFlag::Hangar, quantity: fill_qty, singleton: false, version: 1, ..Default::default()
                        };
                        snap.items.insert(new_item_id, rec.clone());
                        delivered_item_records.push(rec);
                        new_item_id
                    };

                    orders_created.push(PyRep::keyval([
                        ("orderID", PyRep::Integer(oid)),
                        ("typeID", PyRep::Integer(fill_tid as i64)),
                        ("quantity", PyRep::Integer(fill_qty as i64)),
                        ("price", PyRep::Real(ask_price.to_wire_f64())),
                        ("itemID", PyRep::Integer(item_id)),
                    ]));
                }

                Ok((purchased_lines, total_qty_bought, total_cost, total_sales_tax, orders_created, item_statuses, buyer_bal_final, seller_acct_updates, delivered_item_records))
            });

            match res {
                Ok((line_count, total_qty, total_cost, total_sales_tax, orders_created, item_statuses, buyer_bal_final, seller_acct_updates, delivered_item_records)) => {
                    for (s_is_corp, s_owner, s_div, s_bal) in seller_acct_updates {
                        super::emit_market_account_change(ctx, s_is_corp, s_owner, s_div, s_bal);
                    }
                    if let Some(b_bal) = buyer_bal_final {
                        super::emit_market_account_change(ctx, false, char_id, 1, b_bal);
                    }
                    for rec in &delivered_item_records {
                        super::emit_market_item_change(ctx, char_id, rec, vec![(3, PyRep::Integer(0))]);
                    }
                    if args.len() >= 3 {
                        Some(Ok(PyRep::List(orders_created)))
                    } else {
                        Some(Ok(PyRep::keyval([
                            ("ordersCreated", PyRep::List(orders_created)),
                            ("itemStatuses", PyRep::List(item_statuses)),
                            ("purchasedCount", PyRep::Integer(line_count)),
                            ("totalQuantity", PyRep::Integer(total_qty)),
                            ("totalCostISK", PyRep::Real(total_cost.to_wire_f64())),
                            ("salesTaxISK", PyRep::Real(total_sales_tax.to_wire_f64())),
                        ])))
                    }
                }
                Err(e) => {
                    if e.code == "CantRepackageDamagedItem" {
                        let count = e.message.parse::<i64>().unwrap_or(1).max(1);
                        crate::services::notify_emitters_skills_trade_godma::emit_sell_repackage_errors(ctx, char_id, "CantRepackageDamagedItem", count);
                    }
                    Some(Err(e))
                }
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

#[cfg(test)] #[path = "market_orders_escrow_tests.rs"]
mod market_orders_escrow_tests;
