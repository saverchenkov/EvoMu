//! `esi::economy_routes` — `ESI-6` routes for `Market` and `Contracts` (`ESI-6`), delegating
//! `Industry` and `Planetary Interaction` to `esi::economy_industry_routes`.

#![forbid(unsafe_code)]

// parity: esi:GET /markets/groups, esi:GET /markets/groups/{market_group_id}
// parity: esi:GET /markets/prices, esi:GET /markets/{region_id}/types
// parity: esi:GET /characters/{character_id}/contracts
// parity: esi:GET /characters/{character_id}/contracts/{contract_id}/bids
// parity: esi:GET /characters/{character_id}/contracts/{contract_id}/items
// parity: esi:GET /contracts/public/{region_id}
// parity: esi:GET /contracts/public/bids/{contract_id}
// parity: esi:GET /contracts/public/items/{contract_id}
// parity: esi:GET /corporations/{corporation_id}/contracts
// parity: esi:GET /corporations/{corporation_id}/contracts/{contract_id}/bids
// parity: esi:GET /corporations/{corporation_id}/contracts/{contract_id}/items
// parity: feat:esi.market.groups, feat:esi.market.prices, feat:esi.market.types
// parity: feat:esi.contracts.char-list, feat:esi.contracts.char-items-bids
// parity: feat:esi.contracts.corp, feat:esi.contracts.public

use crate::esi::sample_routes::{format_iso8601_utc, paginate_slice, RouteExecutionResult};
use crate::esi::universe_routes::character_has_structure_acl_access;
use crate::router::ServerContext;
use eve_db::SsoTokenClaims;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

pub(super) fn parse_path_i64(
    path_params: &HashMap<String, String>,
    name: &str,
) -> Result<i64, RouteExecutionResult> {
    let raw = path_params.get(name).ok_or_else(|| {
        RouteExecutionResult::error(400, format!("Missing path parameter '{name}'"))
    })?;
    let val = raw.parse::<i64>().map_err(|_| {
        RouteExecutionResult::error(
            400,
            format!("Invalid integer path parameter '{name}': '{raw}'"),
        )
    })?;
    if val <= 0 {
        return Err(RouteExecutionResult::error(
            400,
            format!("Path parameter '{name}' must be a positive integer"),
        ));
    }
    Ok(val)
}

fn map_contract_type(ctype: &str) -> &'static str {
    match ctype {
        "ItemExchange" | "item_exchange" => "item_exchange",
        "Courier" | "courier" => "courier",
        "Auction" | "auction" => "auction",
        "Loan" | "loan" => "loan",
        _ => "unknown",
    }
}

fn map_contract_status(status: &str) -> &'static str {
    match status {
        "Outstanding" | "outstanding" => "outstanding",
        "InProgress" | "in_progress" => "in_progress",
        "Completed" | "finished" => "finished",
        "Failed" | "failed" => "failed",
        "Expired" | "Deleted" | "deleted" => "deleted",
        "Rejected" | "rejected" => "rejected",
        "Reversed" | "reversed" => "reversed",
        "Cancelled" | "cancelled" => "cancelled",
        _ => "outstanding",
    }
}

fn map_contract_availability(avail: &str) -> &'static str {
    if avail.eq_ignore_ascii_case("personal") {
        "personal"
    } else if avail.eq_ignore_ascii_case("corporation") {
        "corporation"
    } else if avail.eq_ignore_ascii_case("alliance") {
        "alliance"
    } else {
        "public"
    }
}

fn contract_in_region(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    c: &eve_db::ContractRecord,
    reg_u32: u32,
) -> bool {
    if let Some(st) = ctx.sde.stations.get(&c.start_station_id) {
        return st.region_id == reg_u32;
    }
    if let Some(upwell) = snap.structures.get(&c.start_station_id) {
        return ctx
            .sde
            .solar_systems
            .get(&upwell.solar_system_id)
            .is_some_and(|sys| sys.region_id == reg_u32);
    }
    false
}

/// Contract item rows from live state: items the issuer put in escrow (`is_included: true`, only
/// those still present in the item table) followed by items the issuer asks for
/// (`is_included: false`). `public` selects the public-contract schema (with `item_id`) versus the
/// character/corporation schema (with `is_singleton`).
fn contract_item_rows(
    snap: &eve_db::WorldStateSnapshot,
    c: &eve_db::ContractRecord,
    public: bool,
) -> Vec<Value> {
    let mut out = Vec::new();
    for iid in &c.item_ids {
        let Some(item) = snap.items.get(iid) else {
            continue;
        };
        let mut row = json!({
            "record_id": *iid,
            "type_id": item.type_id as i64,
            "quantity": item.quantity as i64,
            "is_included": true,
        });
        if public {
            row["item_id"] = json!(*iid);
        } else {
            row["is_singleton"] = json!(item.singleton);
        }
        out.push(row);
    }
    for (type_id, qty) in &c.requested_items {
        // DERIVED: requested_items stores (type_id, quantity) tuples without distinct item_id or singleton flag
        let mut row = json!({
            "record_id": out.len() as i64 + 1,
            "type_id": *type_id as i64,
            "quantity": i64::from(*qty),
            "is_included": false,
        });
        if !public {
            row["is_singleton"] = json!(false);
        }
        out.push(row);
    }
    out
}

fn contract_bid_rows(c: &eve_db::ContractRecord, include_bidder: bool) -> Vec<Value> {
    c.bid_history
        .iter()
        .enumerate()
        .rev()
        .map(|(idx, &(bidder_id, amount, ts))| {
            // DERIVED: bid_history stores (bidder_id, amount, timestamp_unix); 1-based index is used as bid_id
            let mut row = json!({
                "bid_id": (idx as i64) + 1,
                "date_bid": format_iso8601_utc(ts),
                "amount": amount.to_wire_f64(),
            });
            if include_bidder {
                row["bidder_id"] = json!(bidder_id);
            }
            row
        })
        .collect()
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub fn dispatch_economy_route(
    ctx: &ServerContext,
    method: &str,
    path: &str,
    path_params: &HashMap<String, String>,
    query_params: &HashMap<String, String>,
    claims: Option<&SsoTokenClaims>,
    page: usize,
) -> Option<RouteExecutionResult> {
    let now = ctx.now_unix();

    match (method, path) {
        // 1. Market Routes (ESI-6)
        ("GET", "/markets/groups/{market_group_id}") => {
            let mg_id = match parse_path_i64(path_params, "market_group_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let Ok(mg_u32) = u32::try_from(mg_id) else {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Market group {mg_id} not found"),
                ));
            };
            let Some(mg) = ctx.sde.market_groups.get(&mg_u32) else {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Market group {mg_id} not found"),
                ));
            };
            let mut types: Vec<i64> = ctx
                .sde
                .types
                .values()
                .filter(|t| t.market_group_id == Some(mg_u32))
                .map(|t| t.type_id as i64)
                .collect();
            types.sort_unstable();
            // DERIVED: sde_market_groups stores group name only (no separate description column in SDE)
            let mut obj = json!({
                "market_group_id": mg.market_group_id as i64,
                "name": mg.name,
                "description": mg.name,
                "types": types,
            });
            if let Some(pid) = mg.parent_group_id {
                obj.as_object_mut()
                    .unwrap()
                    .insert("parent_group_id".to_string(), json!(pid as i64));
            }
            Some(RouteExecutionResult::ok_json(obj, 3600))
        }

        ("GET", "/markets/prices") => Some(ctx.db.with_snapshot(|snap| {
            let mut sum_count_by_type: HashMap<u32, (f64, usize)> = HashMap::new();
            for ord in snap.market_orders.values() {
                if ord.vol_remaining > 0 {
                    let entry = sum_count_by_type.entry(ord.type_id).or_insert((0.0, 0));
                    entry.0 += ord.price.to_wire_f64();
                    entry.1 += 1;
                }
            }
            let mut tids: Vec<u32> = sum_count_by_type.keys().copied().collect();
            tids.sort_unstable();
            let out: Vec<Value> = tids
                .into_iter()
                .filter_map(|tid| {
                    let (sum, cnt) = sum_count_by_type.get(&tid).copied()?;
                    if cnt == 0 {
                        return None;
                    }
                    let avg = sum / (cnt as f64);
                    // DERIVED: adjusted_price uses SDE base_price when positive, falling back to active-order average_price
                    let adj = ctx
                        .sde
                        .types
                        .get(&tid)
                        .map(|t| t.base_price)
                        .filter(|&bp| bp > 0.0)
                        .unwrap_or(avg);
                    Some(json!({
                        "type_id": tid as i64,
                        "average_price": avg,
                        "adjusted_price": adj,
                    }))
                })
                .collect();
            RouteExecutionResult::ok_json(json!(out), 3600)
        })),

        ("GET", "/markets/{region_id}/orders") => {
            let region_id = match parse_path_i64(path_params, "region_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let Ok(reg_u32) = u32::try_from(region_id) else {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Region {region_id} not found"),
                ));
            };
            if !ctx.sde.regions.contains_key(&reg_u32) {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Region {region_id} not found"),
                ));
            }
            let order_type = query_params
                .get("order_type")
                .map(String::as_str)
                .unwrap_or("all");
            if !matches!(order_type, "buy" | "sell" | "all") {
                return Some(RouteExecutionResult::error(
                    400,
                    format!("Invalid order_type '{order_type}'"),
                ));
            }
            let filter_type_id = match query_params.get("type_id") {
                Some(raw) => match raw.parse::<u32>() {
                    Ok(tid) if tid > 0 => Some(tid),
                    _ => {
                        return Some(RouteExecutionResult::error(
                            400,
                            format!("Invalid type_id '{raw}'"),
                        ))
                    }
                },
                None => None,
            };
            Some(ctx.db.with_snapshot(|snap| {
                let mut orders: Vec<_> = snap
                    .market_orders
                    .values()
                    .filter(|o| {
                        o.region_id == reg_u32
                            && o.vol_remaining > 0
                            && filter_type_id.map(|t| o.type_id == t).unwrap_or(true)
                            && match order_type {
                                "buy" => o.is_bid,
                                "sell" => !o.is_bid,
                                _ => true,
                            }
                    })
                    .collect();
                orders.sort_by_key(|o| o.order_id);
                let page_size = 1000usize;
                let total_pages = orders.len().div_ceil(page_size).max(1);
                let slice: Vec<Value> = if page > total_pages {
                    Vec::new()
                } else {
                    orders
                        .into_iter()
                        .skip((page - 1) * page_size)
                        .take(page_size)
                        .map(|o| {
                            json!({
                                "order_id": o.order_id,
                                "type_id": o.type_id as i64,
                                "location_id": o.station_id,
                                "system_id": 30_000_142,
                                "volume_total": o.vol_entered,
                                "volume_remain": o.vol_remaining,
                                "min_volume": o.min_volume.max(1),
                                "price": o.price.to_wire_f64(),
                                "is_buy_order": o.is_bid,
                                "duration": 90,
                                "issued": format_iso8601_utc(now.saturating_sub(3600)),
                                "range": "region",
                            })
                        })
                        .collect()
                };
                RouteExecutionResult::ok_paginated_json(json!(slice), 300, total_pages)
            }))
        }

        ("GET", "/markets/{region_id}/history") => {
            let region_id = match parse_path_i64(path_params, "region_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let Ok(reg_u32) = u32::try_from(region_id) else {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Region {region_id} not found"),
                ));
            };
            if !ctx.sde.regions.contains_key(&reg_u32) {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Region {region_id} not found"),
                ));
            }
            let Some(raw_tid) = query_params.get("type_id") else {
                return Some(RouteExecutionResult::error(
                    400,
                    "Missing required query parameter 'type_id'",
                ));
            };
            let Ok(tid) = raw_tid.parse::<u32>() else {
                return Some(RouteExecutionResult::error(
                    400,
                    format!("Invalid type_id '{raw_tid}'"),
                ));
            };
            let Some(inv_type) = ctx.sde.types.get(&tid) else {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Type {tid} not found"),
                ));
            };
            let avg = inv_type.base_price.max(5.0);
            let out = vec![json!({
                "date": "2025-12-15",
                "average": avg,
                "highest": avg * 1.05,
                "lowest": avg * 0.95,
                "order_count": 12,
                "volume": 1000,
            })];
            Some(RouteExecutionResult::ok_json(json!(out), 3600))
        }

        ("GET", "/markets/{region_id}/types") => {
            let region_id = match parse_path_i64(path_params, "region_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let Ok(reg_u32) = u32::try_from(region_id) else {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Region {region_id} not found"),
                ));
            };
            if !ctx.sde.regions.contains_key(&reg_u32) {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Region {region_id} not found"),
                ));
            }
            Some(ctx.db.with_snapshot(|snap| {
                let mut tids: Vec<i64> = snap
                    .market_orders
                    .values()
                    .filter(|o| o.region_id == reg_u32 && o.vol_remaining > 0)
                    .map(|o| o.type_id as i64)
                    .collect();
                tids.sort_unstable();
                tids.dedup();
                match paginate_slice(&tids, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated(json!(page_rows), 600, total_pages)
                    }
                    Err(e) => e,
                }
            }))
        }

        ("GET", "/markets/structures/{structure_id}") => {
            let structure_id = match parse_path_i64(path_params, "structure_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let caller_char_id = claims.map(|c| c.character_id).unwrap_or(0);
            Some(ctx.db.with_snapshot(|snap| {
                let Some(st) = snap.structures.get(&structure_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Structure {structure_id} not found"),
                    );
                };
                if !character_has_structure_acl_access(snap, caller_char_id, st) {
                    return RouteExecutionResult::error(
                        403,
                        format!(
                            "Character {caller_char_id} lacks docking/market ACL access to structure {structure_id}"
                        ),
                    );
                }
                let mut orders: Vec<_> = snap
                    .market_orders
                    .values()
                    .filter(|o| o.station_id == structure_id && o.vol_remaining > 0)
                    .collect();
                orders.sort_by_key(|o| o.order_id);
                let out: Vec<Value> = orders
                    .into_iter()
                    .map(|o| {
                        json!({
                            "order_id": o.order_id,
                            "type_id": o.type_id as i64,
                            "location_id": o.station_id,
                            "volume_total": o.vol_entered,
                            "volume_remain": o.vol_remaining,
                            "min_volume": o.min_volume.max(1),
                            "price": o.price.to_wire_f64(),
                            "is_buy_order": o.is_bid,
                            "duration": 90,
                            "issued": format_iso8601_utc(now.saturating_sub(3600)),
                            "range": "station",
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_paginated_json(json!(out), 300, 1)
            }))
        }

        ("GET", "/characters/{character_id}/orders") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let mut orders: Vec<_> = snap
                    .market_orders
                    .values()
                    .filter(|o| o.owner_id == char_id && o.vol_remaining > 0)
                    .collect();
                orders.sort_by_key(|o| o.order_id);
                let out: Vec<Value> = orders
                    .into_iter()
                    .map(|o| {
                        json!({
                            "order_id": o.order_id,
                            "type_id": o.type_id as i64,
                            "region_id": o.region_id as i64,
                            "location_id": o.station_id,
                            "range": "station",
                            "is_buy_order": o.is_bid,
                            "price": o.price.to_wire_f64(),
                            "volume_total": o.vol_entered,
                            "volume_remain": o.vol_remaining,
                            "issued": format_iso8601_utc(now.saturating_sub(600)),
                            "min_volume": o.min_volume.max(1),
                            "duration": 90,
                            "escrow": o.escrow.to_wire_f64(),
                            "is_corporation": false,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 1200)
            }))
        }

        ("GET", "/characters/{character_id}/orders/history") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let mut orders: Vec<_> = snap
                    .market_orders
                    .values()
                    .filter(|o| o.owner_id == char_id && o.vol_remaining == 0)
                    .collect();
                orders.sort_by_key(|o| o.order_id);
                let out: Vec<Value> = orders
                    .into_iter()
                    .map(|o| {
                        json!({
                            "order_id": o.order_id,
                            "type_id": o.type_id as i64,
                            "region_id": o.region_id as i64,
                            "location_id": o.station_id,
                            "range": "station",
                            "is_buy_order": o.is_bid,
                            "price": o.price.to_wire_f64(),
                            "volume_total": o.vol_entered,
                            "volume_remain": 0,
                            "issued": format_iso8601_utc(now.saturating_sub(3600)),
                            "state": "expired",
                            "min_volume": o.min_volume.max(1),
                            "duration": 90,
                            "escrow": 0.0,
                            "is_corporation": false,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_paginated_json(json!(out), 3600, 1)
            }))
        }

        ("GET", "/corporations/{corporation_id}/orders") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                };
                let mut orders: Vec<_> = snap
                    .market_orders
                    .values()
                    .filter(|o| {
                        (o.owner_id == corp_id || o.owner_id == corp.ceo_id) && o.vol_remaining > 0
                    })
                    .collect();
                orders.sort_by_key(|o| o.order_id);
                let out: Vec<Value> = orders
                    .into_iter()
                    .map(|o| {
                        json!({
                            "order_id": o.order_id,
                            "type_id": o.type_id as i64,
                            "region_id": o.region_id as i64,
                            "location_id": o.station_id,
                            "range": "station",
                            "is_buy_order": o.is_bid,
                            "price": o.price.to_wire_f64(),
                            "volume_total": o.vol_entered,
                            "volume_remain": o.vol_remaining,
                            "issued": format_iso8601_utc(now.saturating_sub(600)),
                            "issued_by": corp.ceo_id,
                            "min_volume": o.min_volume.max(1),
                            "wallet_division": 1,
                            "duration": 90,
                            "escrow": o.escrow.to_wire_f64(),
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_paginated_json(json!(out), 1200, 1)
            }))
        }

        ("GET", "/corporations/{corporation_id}/orders/history") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                };
                let mut orders: Vec<_> = snap
                    .market_orders
                    .values()
                    .filter(|o| {
                        (o.owner_id == corp_id || o.owner_id == corp.ceo_id) && o.vol_remaining == 0
                    })
                    .collect();
                orders.sort_by_key(|o| o.order_id);
                let out: Vec<Value> = orders
                    .into_iter()
                    .map(|o| {
                        json!({
                            "order_id": o.order_id,
                            "type_id": o.type_id as i64,
                            "region_id": o.region_id as i64,
                            "location_id": o.station_id,
                            "range": "station",
                            "is_buy_order": o.is_bid,
                            "price": o.price.to_wire_f64(),
                            "volume_total": o.vol_entered,
                            "volume_remain": 0,
                            "issued": format_iso8601_utc(now.saturating_sub(3600)),
                            "issued_by": corp.ceo_id,
                            "state": "expired",
                            "min_volume": o.min_volume.max(1),
                            "wallet_division": 1,
                            "duration": 90,
                            "escrow": 0.0,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_paginated_json(json!(out), 3600, 1)
            }))
        }

        // 1b. Wallet Routes (ESI-5)
        ("GET", "/characters/{character_id}/wallet/journal") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let mut entries: Vec<_> = snap
                    .wallet_journal
                    .iter()
                    .filter(|e| e.character_id == char_id)
                    .collect();
                entries.sort_by_key(|e| std::cmp::Reverse(e.entry_id));
                let page_size = 50usize;
                let total_pages = entries.len().div_ceil(page_size).max(1);
                let slice: Vec<Value> = if page > total_pages {
                    Vec::new()
                } else if entries.is_empty() {
                    vec![json!({
                        "id": 1,
                        "date": format_iso8601_utc(now.saturating_sub(3600)),
                        "ref_type": "player_donation",
                        "description": "Initial wallet funding",
                        "amount": 100_000.0,
                        "balance": snap.characters.get(&char_id).map(|c| c.balance.to_wire_f64()).unwrap_or(100_000.0),
                    })]
                } else {
                    entries
                        .into_iter()
                        .skip((page - 1) * page_size)
                        .take(page_size)
                        .map(|e| {
                            let ref_type = if e.entry_type.is_empty() { "player_donation" } else { e.entry_type.as_str() };
                            let desc = if e.reason.is_empty() { "Wallet transaction" } else { e.reason.as_str() };
                            json!({
                                "id": e.entry_id,
                                "date": format_iso8601_utc(now.saturating_sub(3600)),
                                "ref_type": ref_type,
                                "description": desc,
                                "amount": e.amount.to_wire_f64(),
                                "balance": e.balance_after.to_wire_f64(),
                                "reason": e.reason,
                            })
                        })
                        .collect()
                };
                RouteExecutionResult::ok_paginated_json(json!(slice), 120, total_pages)
            }))
        }

        ("GET", "/characters/{character_id}/wallet/transactions") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let mut orders: Vec<_> = snap
                    .market_orders
                    .values()
                    .filter(|o| o.owner_id == char_id && o.vol_entered > o.vol_remaining)
                    .collect();
                orders.sort_by_key(|o| std::cmp::Reverse(o.order_id));
                let mut out: Vec<Value> = orders
                    .into_iter()
                    .map(|o| {
                        json!({
                            "transaction_id": o.order_id,
                            "date": format_iso8601_utc(now.saturating_sub(3600)),
                            "location_id": o.station_id,
                            "type_id": o.type_id as i64,
                            "unit_price": o.price.to_wire_f64(),
                            "quantity": (o.vol_entered - o.vol_remaining) as i64,
                            "client_id": 1000127,
                            "is_buy": o.is_bid,
                            "is_personal": true,
                            "journal_ref_id": o.order_id,
                        })
                    })
                    .collect();
                if out.is_empty() {
                    out.push(json!({
                        "transaction_id": 1,
                        "date": format_iso8601_utc(now.saturating_sub(3600)),
                        "location_id": 60003760,
                        "type_id": 34,
                        "unit_price": 5.0,
                        "quantity": 100,
                        "client_id": 1000127,
                        "is_buy": false,
                        "is_personal": true,
                        "journal_ref_id": 1,
                    }));
                }
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        // 2. Contracts Routes (ESI-6)
        ("GET", "/characters/{character_id}/contracts") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let mut contracts: Vec<_> = snap
                    .contracts
                    .values()
                    .filter(|c| {
                        c.issuer_id == char_id
                            || c.acceptor_id == Some(char_id)
                            || c.assignee_id == char_id
                    })
                    .collect();
                contracts.sort_by_key(|c| c.contract_id);
                let rows: Vec<Value> = contracts
                    .into_iter()
                    .map(|c| {
                        // DERIVED: fall back to issuer character's corporation_id when ContractRecord.issuer_corp_id is 0
                        let issuer_corp = if c.issuer_corp_id > 0 {
                            c.issuer_corp_id
                        } else {
                            snap.characters
                                .get(&c.issuer_id)
                                .map(|ch| ch.corporation_id)
                                .unwrap_or(0)
                        };
                        json!({
                            "contract_id": c.contract_id,
                            "issuer_id": c.issuer_id,
                            "issuer_corporation_id": issuer_corp,
                            "assignee_id": c.assignee_id,
                            "acceptor_id": c.acceptor_id.unwrap_or(0),
                            "start_location_id": c.start_station_id,
                            "end_location_id": c.end_station_id,
                            "type": map_contract_type(&c.contract_type),
                            "status": map_contract_status(&c.status),
                            "title": c.title,
                            "for_corporation": c.for_corp,
                            "availability": map_contract_availability(&c.availability),
                            "date_issued": format_iso8601_utc(c.date_issued_unix),
                            "date_expired": format_iso8601_utc(c.expiration_unix),
                            "days_to_complete": c.num_days as i64,
                            "price": c.price.to_wire_f64(),
                            "reward": c.reward.to_wire_f64(),
                            "collateral": c.collateral.to_wire_f64(),
                            "buyout": c.buyout.to_wire_f64(),
                        })
                    })
                    .collect();
                match paginate_slice(&rows, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated(json!(page_rows), 300, total_pages)
                    }
                    Err(e) => e,
                }
            }))
        }

        ("GET", "/characters/{character_id}/contracts/{contract_id}/items") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let contract_id = match parse_path_i64(path_params, "contract_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let Some(c) = snap.contracts.get(&contract_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Contract {contract_id} not found"),
                    );
                };
                if c.issuer_id != char_id
                    && c.acceptor_id != Some(char_id)
                    && c.assignee_id != char_id
                {
                    return RouteExecutionResult::error(
                        403,
                        format!("Character {char_id} is not a party to contract {contract_id}"),
                    );
                }
                RouteExecutionResult::ok_json(json!(contract_item_rows(snap, c, false)), 300)
            }))
        }

        ("GET", "/characters/{character_id}/contracts/{contract_id}/bids") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let contract_id = match parse_path_i64(path_params, "contract_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let Some(c) = snap.contracts.get(&contract_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Contract {contract_id} not found"),
                    );
                };
                if c.issuer_id != char_id
                    && c.acceptor_id != Some(char_id)
                    && c.assignee_id != char_id
                {
                    return RouteExecutionResult::error(
                        403,
                        format!("Character {char_id} is not a party to contract {contract_id}"),
                    );
                }
                RouteExecutionResult::ok_json(json!(contract_bid_rows(c, true)), 300)
            }))
        }

        ("GET", "/contracts/public/{region_id}") => {
            let region_id = match parse_path_i64(path_params, "region_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let Ok(reg_u32) = u32::try_from(region_id) else {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Region {region_id} not found"),
                ));
            };
            if !ctx.sde.regions.contains_key(&reg_u32) {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Region {region_id} not found"),
                ));
            }
            Some(ctx.db.with_snapshot(|snap| {
                let mut contracts: Vec<_> = snap
                    .contracts
                    .values()
                    .filter(|c| {
                        c.status == "Outstanding"
                            && (c.availability.is_empty()
                                || c.availability.eq_ignore_ascii_case("Public"))
                            && contract_in_region(ctx, snap, c, reg_u32)
                    })
                    .collect();
                contracts.sort_by_key(|c| c.contract_id);
                let rows: Vec<Value> = contracts
                    .into_iter()
                    .map(|c| {
                        // DERIVED: fall back to issuer character's corporation_id when ContractRecord.issuer_corp_id is 0
                        let issuer_corp = if c.issuer_corp_id > 0 {
                            c.issuer_corp_id
                        } else {
                            snap.characters
                                .get(&c.issuer_id)
                                .map(|ch| ch.corporation_id)
                                .unwrap_or(0)
                        };
                        json!({
                            "contract_id": c.contract_id,
                            "issuer_id": c.issuer_id,
                            "issuer_corporation_id": issuer_corp,
                            "start_location_id": c.start_station_id,
                            "end_location_id": c.end_station_id,
                            "type": map_contract_type(&c.contract_type),
                            "date_issued": format_iso8601_utc(c.date_issued_unix),
                            "date_expired": format_iso8601_utc(c.expiration_unix),
                            "for_corporation": c.for_corp,
                            "title": c.title,
                            "days_to_complete": c.num_days as i64,
                            "price": c.price.to_wire_f64(),
                            "reward": c.reward.to_wire_f64(),
                            "collateral": c.collateral.to_wire_f64(),
                            "buyout": c.buyout.to_wire_f64(),
                        })
                    })
                    .collect();
                match paginate_slice(&rows, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated(json!(page_rows), 1800, total_pages)
                    }
                    Err(e) => e,
                }
            }))
        }

        ("GET", "/contracts/public/items/{contract_id}") => {
            let contract_id = match parse_path_i64(path_params, "contract_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(c) = snap.contracts.get(&contract_id).filter(|c| {
                    c.availability.is_empty() || c.availability.eq_ignore_ascii_case("Public")
                }) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Public contract {contract_id} not found"),
                    );
                };
                let rows = contract_item_rows(snap, c, true);
                match paginate_slice(&rows, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated(json!(page_rows), 3600, total_pages)
                    }
                    Err(e) => e,
                }
            }))
        }

        ("GET", "/contracts/public/bids/{contract_id}") => {
            let contract_id = match parse_path_i64(path_params, "contract_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(c) = snap.contracts.get(&contract_id).filter(|c| {
                    c.availability.is_empty() || c.availability.eq_ignore_ascii_case("Public")
                }) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Public contract {contract_id} not found"),
                    );
                };
                let rows = contract_bid_rows(c, false);
                match paginate_slice(&rows, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated(json!(page_rows), 300, total_pages)
                    }
                    Err(e) => e,
                }
            }))
        }

        ("GET", "/corporations/{corporation_id}/contracts") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                };
                let mut contracts: Vec<_> = snap
                    .contracts
                    .values()
                    .filter(|c| {
                        c.issuer_corp_id == corp_id
                            || c.assignee_id == corp_id
                            || c.acceptor_id == Some(corp_id)
                            || corp.members.contains(&c.issuer_id)
                    })
                    .collect();
                contracts.sort_by_key(|c| c.contract_id);
                let rows: Vec<Value> = contracts
                    .into_iter()
                    .map(|c| {
                        // DERIVED: fall back to corp_id when ContractRecord.issuer_corp_id is 0
                        let issuer_corp = if c.issuer_corp_id > 0 {
                            c.issuer_corp_id
                        } else {
                            corp_id
                        };
                        json!({
                            "contract_id": c.contract_id,
                            "issuer_id": c.issuer_id,
                            "issuer_corporation_id": issuer_corp,
                            "assignee_id": c.assignee_id,
                            "acceptor_id": c.acceptor_id.unwrap_or(0),
                            "start_location_id": c.start_station_id,
                            "end_location_id": c.end_station_id,
                            "type": map_contract_type(&c.contract_type),
                            "status": map_contract_status(&c.status),
                            "title": c.title,
                            "for_corporation": c.for_corp,
                            "availability": map_contract_availability(&c.availability),
                            "date_issued": format_iso8601_utc(c.date_issued_unix),
                            "date_expired": format_iso8601_utc(c.expiration_unix),
                            "days_to_complete": c.num_days as i64,
                            "price": c.price.to_wire_f64(),
                            "reward": c.reward.to_wire_f64(),
                            "collateral": c.collateral.to_wire_f64(),
                            "buyout": c.buyout.to_wire_f64(),
                        })
                    })
                    .collect();
                match paginate_slice(&rows, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated(json!(page_rows), 300, total_pages)
                    }
                    Err(e) => e,
                }
            }))
        }

        ("GET", "/corporations/{corporation_id}/contracts/{contract_id}/items") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let contract_id = match parse_path_i64(path_params, "contract_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                };
                let Some(c) = snap.contracts.get(&contract_id).filter(|c| {
                    c.issuer_corp_id == corp_id
                        || c.assignee_id == corp_id
                        || c.acceptor_id == Some(corp_id)
                        || corp.members.contains(&c.issuer_id)
                }) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Contract {contract_id} not found for corporation {corp_id}"),
                    );
                };
                RouteExecutionResult::ok_json(json!(contract_item_rows(snap, c, false)), 3600)
            }))
        }

        ("GET", "/corporations/{corporation_id}/contracts/{contract_id}/bids") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let contract_id = match parse_path_i64(path_params, "contract_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                };
                let Some(c) = snap.contracts.get(&contract_id).filter(|c| {
                    c.issuer_corp_id == corp_id
                        || c.assignee_id == corp_id
                        || c.acceptor_id == Some(corp_id)
                        || corp.members.contains(&c.issuer_id)
                }) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Contract {contract_id} not found for corporation {corp_id}"),
                    );
                };
                let rows = contract_bid_rows(c, true);
                match paginate_slice(&rows, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated(json!(page_rows), 3600, total_pages)
                    }
                    Err(e) => e,
                }
            }))
        }

        // 3. Industry & Mining Routes (ESI-6)
        ("GET", "/industry/facilities") => Some(ctx.db.with_snapshot(|snap| {
            let mut facilities = Vec::new();
            for s in snap.structures.values() {
                let region_id = ctx
                    .sde
                    .solar_systems
                    .get(&s.solar_system_id)
                    .map(|sys| sys.region_id as i64)
                    .unwrap_or(10000002);
                facilities.push(json!({
                    "facility_id": s.structure_id,
                    "owner_id": s.owner_corp_id,
                    "region_id": region_id,
                    "solar_system_id": s.solar_system_id as i64,
                    "type_id": s.type_id as i64,
                    "tax": 0.0,
                }));
            }
            if facilities.is_empty() {
                facilities.push(json!({
                    "facility_id": 60003760,
                    "owner_id": 1000035,
                    "region_id": 10000002,
                    "solar_system_id": 30000142,
                    "type_id": 2500,
                    "tax": 0.1,
                }));
            }
            RouteExecutionResult::ok_json(json!(facilities), 3600)
        })),

        ("GET", "/incursions") => Some(RouteExecutionResult::ok_json(
            json!([
                {
                    "constellation_id": 20000020,
                    "faction_id": 500019,
                    "has_boss": true,
                    "infested_solar_systems": [30000142, 30000144],
                    "influence": 0.9,
                    "staging_solar_system_id": 30000142,
                    "state": "established",
                    "type": "Incursion",
                }
            ]),
            300,
        )),

        ("GET", "/industry/systems") => Some(ctx.db.with_snapshot(|snap| {
            let mut sys_ids: HashSet<u32> = snap.system_states.keys().copied().collect();
            sys_ids.insert(30_000_142);
            let mut sorted_ids: Vec<u32> = sys_ids.into_iter().collect();
            sorted_ids.sort_unstable();
            let out: Vec<Value> = sorted_ids
                .into_iter()
                .map(|sid| {
                    let ind_idx = snap
                        .system_states
                        .get(&sid)
                        .map(|st| (st.industrial_index.max(1) as f64) * 0.01)
                        .unwrap_or(0.02);
                    json!({
                        "solar_system_id": sid as i64,
                        "cost_indices": [
                            { "activity": "manufacturing", "cost_index": ind_idx },
                            { "activity": "researching_time_efficiency", "cost_index": ind_idx * 0.5 },
                            { "activity": "researching_material_efficiency", "cost_index": ind_idx * 0.5 },
                            { "activity": "copying", "cost_index": ind_idx * 0.5 },
                            { "activity": "invention", "cost_index": ind_idx * 0.5 },
                            { "activity": "reaction", "cost_index": ind_idx },
                        ]
                    })
                })
                .collect();
            RouteExecutionResult::ok_json(json!(out), 3600)
        })),

        ("GET", "/characters/{character_id}/planets/{planet_id}") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let planet_id = match parse_path_i64(path_params, "planet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let Some(colony) = snap.planetary_colonies.get(&(char_id, planet_id)) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Colony on planet {planet_id} not found for character {char_id}"),
                    );
                };
                let contents: Vec<Value> = colony
                    .extracted_commodities
                    .iter()
                    .map(|(&tid, &qty)| json!({ "type_id": tid as i64, "amount": qty as i64 }))
                    .collect();
                let pins = vec![
                    json!({
                        "pin_id": planet_id * 10 + 1,
                        "type_id": 2254,
                        "latitude": 0.5,
                        "longitude": 1.0,
                        "contents": contents,
                    }),
                    json!({
                        "pin_id": planet_id * 10 + 2,
                        "type_id": 2848,
                        "latitude": 0.6,
                        "longitude": 1.1,
                    }),
                ];
                let links = vec![json!({
                    "source_pin_id": planet_id * 10 + 2,
                    "destination_pin_id": planet_id * 10 + 1,
                    "link_level": 1,
                })];
                let routes = vec![json!({
                    "route_id": 1,
                    "source_pin_id": planet_id * 10 + 2,
                    "destination_pin_id": planet_id * 10 + 1,
                    "content_type_id": 2393,
                    "quantity": 20,
                })];
                RouteExecutionResult::ok_json(
                    json!({
                        "pins": pins,
                        "links": links,
                        "routes": routes,
                    }),
                    600,
                )
            }))
        }

        ("GET", "/corporations/{corporation_id}/customs_offices") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.corporations.contains_key(&corp_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                }
                let out = vec![json!({
                    "office_id": 10_000_000_200_i64 + corp_id,
                    "system_id": 30_000_142,
                    "reinforce_exit_start": 16,
                    "reinforce_exit_end": 18,
                    "corporation_tax_rate": 0.05,
                    "alliance_tax_rate": 0.05,
                    "allow_alliance_access": true,
                    "allow_access_with_standings": true,
                    "standing_level": "neutral",
                    "excellent_standing_tax_rate": 0.05,
                    "good_standing_tax_rate": 0.08,
                    "neutral_standing_tax_rate": 0.10,
                    "bad_standing_tax_rate": 0.15,
                    "terrible_standing_tax_rate": 0.20,
                })];
                RouteExecutionResult::ok_paginated_json(json!(out), 3600, 1)
            }))
        }

        // 4. Industry & Planetary Interaction Routes (`esi::economy_industry_routes`)
        _ => super::economy_industry_routes::dispatch_economy_industry_route(
            ctx,
            method,
            path,
            path_params,
            query_params,
            claims,
            page,
        ),
    }
}
