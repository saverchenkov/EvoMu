//! `esi::economy_routes` — `ESI-6` routes for `Market` and `Contracts` (`ESI-6`), delegating
//! `Industry` and `Planetary Interaction` to `esi::economy_industry_routes`.

#![forbid(unsafe_code)]

// parity: esi:GET /markets/groups, esi:GET /markets/groups/{market_group_id}
// parity: esi:GET /markets/prices, esi:GET /markets/{region_id}/types
// parity: esi:GET /markets/{region_id}/orders, esi:GET /markets/{region_id}/history
// parity: esi:GET /markets/structures/{structure_id}
// parity: esi:GET /characters/{character_id}/orders, esi:GET /characters/{character_id}/orders/history
// parity: esi:GET /corporations/{corporation_id}/orders, esi:GET /corporations/{corporation_id}/orders/history
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
// parity: feat:esi.market.region-orders, feat:esi.market.history, feat:esi.market.structure-orders
// parity: feat:esi.market.char-orders, feat:esi.market.corp-orders
// parity: feat:esi.contracts.char-list, feat:esi.contracts.char-items-bids
// parity: feat:esi.contracts.corp, feat:esi.contracts.public
// parity: plan:ESI-6

use crate::esi::sample_routes::{format_iso8601_utc, paginate_slice, RouteExecutionResult};
use crate::esi::universe_routes::character_has_structure_acl_access;
use crate::router::ServerContext;
use eve_db::SsoTokenClaims;
use serde_json::{json, Value};
use std::collections::HashMap;

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

fn is_character_party_to_contract(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    c: &eve_db::ContractRecord,
) -> bool {
    if c.issuer_id == char_id
        || c.acceptor_id == Some(char_id)
        || c.assignee_id == char_id
        || c.current_bidder_id == Some(char_id)
        || c.bid_history.iter().any(|&(b, _, _)| b == char_id)
        || ((c.availability.is_empty() || c.availability.eq_ignore_ascii_case("Public"))
            && c.assignee_id == 0)
    {
        return true;
    }
    snap.characters.get(&char_id).is_some_and(|ch| {
        (ch.corporation_id != 0
            && (c.assignee_id == ch.corporation_id
                || (c.availability.eq_ignore_ascii_case("Corporation")
                    && c.issuer_corp_id == ch.corporation_id)))
            || ch.alliance_id.is_some_and(|a| a != 0 && c.assignee_id == a)
    })
}

fn is_corp_contract(
    corp: &eve_db::CorporationRecord,
    corp_id: i64,
    c: &eve_db::ContractRecord,
) -> bool {
    (c.for_corp
        && (c.issuer_corp_id == corp_id
            || (c.issuer_corp_id == 0 && corp.members.contains(&c.issuer_id))))
        || c.assignee_id == corp_id
        || c.acceptor_id == Some(corp_id)
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
    let _now = ctx.now_unix();

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
            let description = ctx
                .sde
                .market_group_description(mg.market_group_id)
                .unwrap_or_default();
            let mut obj = json!({
                "market_group_id": mg.market_group_id as i64,
                "name": mg.name,
                "description": description,
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
                let mut rows = Vec::new();
                for o in orders {
                    let Some(meta) = snap.market_order_meta.get(&o.order_id) else {
                        continue;
                    };
                    let Some(system_id) = ctx
                        .sde
                        .stations
                        .get(&o.station_id)
                        .map(|st| st.solar_system_id as i64)
                        .or_else(|| {
                            snap.structures
                                .get(&o.station_id)
                                .map(|st| st.solar_system_id as i64)
                        })
                    else {
                        continue;
                    };
                    rows.push(json!({
                        "order_id": o.order_id,
                        "type_id": o.type_id as i64,
                        "location_id": o.station_id,
                        "system_id": system_id,
                        "volume_total": o.vol_entered,
                        "volume_remain": o.vol_remaining,
                        "min_volume": o.min_volume,
                        "price": o.price.to_wire_f64(),
                        "is_buy_order": o.is_bid,
                        "duration": meta.duration_days,
                        "issued": format_iso8601_utc(meta.issued_unix),
                        "range": meta.range,
                    }));
                }
                match paginate_slice(&rows, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated(json!(page_rows), 300, total_pages)
                    }
                    Err(e) => e,
                }
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
            if !ctx.sde.types.contains_key(&tid) {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Type {tid} not found"),
                ));
            }
            Some(ctx.db.with_snapshot(|snap| {
                let mut entries = snap
                    .market_history
                    .get(&(reg_u32, tid))
                    .cloned()
                    .unwrap_or_default();
                entries.sort_by(|a, b| a.date.cmp(&b.date));
                let out: Vec<Value> = entries
                    .into_iter()
                    .map(|d| {
                        json!({
                            "date": d.date,
                            "average": d.average,
                            "highest": d.highest,
                            "lowest": d.lowest,
                            "order_count": d.order_count,
                            "volume": d.volume,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
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
                if !character_has_structure_acl_access(snap, caller_char_id, st)
                    || !crate::services::station_structure::structure_upwell_ops::evaluate_character_structure_access_in_snap(
                        snap,
                        structure_id,
                        caller_char_id,
                    )
                {
                    return RouteExecutionResult::error(
                        403,
                        format!(
                            "Character {caller_char_id} lacks docking ACL access to structure {structure_id}"
                        ),
                    );
                }
                if !crate::services::station_structure::structure_upwell_ops::structure_allows_market_orders(
                    ctx,
                    snap,
                    structure_id,
                ) {
                    return RouteExecutionResult::error(
                        403,
                        format!(
                            "Structure {structure_id} does not have an online Standup Market Hub I"
                        ),
                    );
                }
                let mut orders: Vec<_> = snap
                    .market_orders
                    .values()
                    .filter(|o| o.station_id == structure_id && o.vol_remaining > 0)
                    .collect();
                orders.sort_by_key(|o| o.order_id);
                let mut rows = Vec::new();
                for o in orders {
                    let Some(meta) = snap.market_order_meta.get(&o.order_id) else {
                        continue;
                    };
                    rows.push(json!({
                        "order_id": o.order_id,
                        "type_id": o.type_id as i64,
                        "location_id": o.station_id,
                        "volume_total": o.vol_entered,
                        "volume_remain": o.vol_remaining,
                        "min_volume": o.min_volume,
                        "price": o.price.to_wire_f64(),
                        "is_buy_order": o.is_bid,
                        "duration": meta.duration_days,
                        "issued": format_iso8601_utc(meta.issued_unix),
                        "range": meta.range,
                    }));
                }
                match paginate_slice(&rows, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated(json!(page_rows), 300, total_pages)
                    }
                    Err(e) => e,
                }
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
                let mut rows = Vec::new();
                for o in orders {
                    let Some(meta) = snap.market_order_meta.get(&o.order_id) else {
                        continue;
                    };
                    rows.push(json!({
                        "order_id": o.order_id,
                        "type_id": o.type_id as i64,
                        "region_id": o.region_id as i64,
                        "location_id": o.station_id,
                        "range": meta.range,
                        "is_buy_order": o.is_bid,
                        "price": o.price.to_wire_f64(),
                        "volume_total": o.vol_entered,
                        "volume_remain": o.vol_remaining,
                        "issued": format_iso8601_utc(meta.issued_unix),
                        "min_volume": o.min_volume,
                        "duration": meta.duration_days,
                        "escrow": o.escrow.to_wire_f64(),
                        "is_corporation": meta.is_corporation,
                    }));
                }
                RouteExecutionResult::ok_json(json!(rows), 1200)
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
                let mut rows = Vec::new();
                for o in orders {
                    let Some(meta) = snap.market_order_meta.get(&o.order_id) else {
                        continue;
                    };
                    let Some(ref state) = meta.state else {
                        continue;
                    };
                    rows.push(json!({
                        "order_id": o.order_id,
                        "type_id": o.type_id as i64,
                        "region_id": o.region_id as i64,
                        "location_id": o.station_id,
                        "range": meta.range,
                        "is_buy_order": o.is_bid,
                        "price": o.price.to_wire_f64(),
                        "volume_total": o.vol_entered,
                        "volume_remain": o.vol_remaining,
                        "issued": format_iso8601_utc(meta.issued_unix),
                        "state": state,
                        "min_volume": o.min_volume,
                        "duration": meta.duration_days,
                        "escrow": o.escrow.to_wire_f64(),
                        "is_corporation": meta.is_corporation,
                    }));
                }
                match paginate_slice(&rows, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated(json!(page_rows), 3600, total_pages)
                    }
                    Err(e) => e,
                }
            }))
        }

        ("GET", "/corporations/{corporation_id}/orders") => {
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
                let mut orders: Vec<_> = snap
                    .market_orders
                    .values()
                    .filter(|o| o.owner_id == corp_id && o.vol_remaining > 0)
                    .collect();
                orders.sort_by_key(|o| o.order_id);
                let mut rows = Vec::new();
                for o in orders {
                    let Some(meta) = snap.market_order_meta.get(&o.order_id) else {
                        continue;
                    };
                    if !meta.is_corporation {
                        continue;
                    }
                    let Some(issued_by) = meta.issued_by else {
                        continue;
                    };
                    let Some(wallet_division) = meta.wallet_division else {
                        continue;
                    };
                    rows.push(json!({
                        "order_id": o.order_id,
                        "type_id": o.type_id as i64,
                        "region_id": o.region_id as i64,
                        "location_id": o.station_id,
                        "range": meta.range,
                        "is_buy_order": o.is_bid,
                        "price": o.price.to_wire_f64(),
                        "volume_total": o.vol_entered,
                        "volume_remain": o.vol_remaining,
                        "issued": format_iso8601_utc(meta.issued_unix),
                        "issued_by": issued_by,
                        "min_volume": o.min_volume,
                        "wallet_division": wallet_division,
                        "duration": meta.duration_days,
                        "escrow": o.escrow.to_wire_f64(),
                    }));
                }
                match paginate_slice(&rows, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated(json!(page_rows), 1200, total_pages)
                    }
                    Err(e) => e,
                }
            }))
        }

        ("GET", "/corporations/{corporation_id}/orders/history") => {
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
                let mut orders: Vec<_> = snap
                    .market_orders
                    .values()
                    .filter(|o| o.owner_id == corp_id && o.vol_remaining == 0)
                    .collect();
                orders.sort_by_key(|o| o.order_id);
                let mut rows = Vec::new();
                for o in orders {
                    let Some(meta) = snap.market_order_meta.get(&o.order_id) else {
                        continue;
                    };
                    if !meta.is_corporation {
                        continue;
                    }
                    let Some(ref state) = meta.state else {
                        continue;
                    };
                    let Some(wallet_division) = meta.wallet_division else {
                        continue;
                    };
                    let mut obj = json!({
                        "order_id": o.order_id,
                        "type_id": o.type_id as i64,
                        "region_id": o.region_id as i64,
                        "location_id": o.station_id,
                        "range": meta.range,
                        "is_buy_order": o.is_bid,
                        "price": o.price.to_wire_f64(),
                        "volume_total": o.vol_entered,
                        "volume_remain": o.vol_remaining,
                        "issued": format_iso8601_utc(meta.issued_unix),
                        "state": state,
                        "min_volume": o.min_volume,
                        "wallet_division": wallet_division,
                        "duration": meta.duration_days,
                        "escrow": o.escrow.to_wire_f64(),
                    });
                    if let Some(issued_by) = meta.issued_by {
                        obj["issued_by"] = json!(issued_by);
                    }
                    rows.push(obj);
                }
                match paginate_slice(&rows, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated(json!(page_rows), 3600, total_pages)
                    }
                    Err(e) => e,
                }
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
                if !is_character_party_to_contract(snap, char_id, c) {
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
                if !is_character_party_to_contract(snap, char_id, c) {
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
                    .filter(|c| is_corp_contract(corp, corp_id, c))
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
                let Some(c) = snap.contracts.get(&contract_id).filter(|c| is_corp_contract(corp, corp_id, c)) else {
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
                let Some(c) = snap.contracts.get(&contract_id).filter(|c| is_corp_contract(corp, corp_id, c)) else {
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

        // 3. Industry & Planetary Interaction Routes (`esi::economy_industry_routes`)
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
