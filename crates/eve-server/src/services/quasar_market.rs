//! `quasar_market` — Quasar V24.01 Market Services (`Q-6`).
//!
//! Implements live backend handlers for market orders, price history, regional active types,
//! and `NewEntryNotice` broadcasting across:
//! - `eve.market.orders.*` (`GetOrdersRequest`, `GetPublicOpenInRegionRequest`,
//!   `GetOpenInStationRequest`, `GetOpenInStructureRequest`, `GetOpenForCharacterRequest`,
//!   `GetHistoryForCharacterRequest`, `GetOpenForCorporationRequest`, `GetHistoryForCorporationRequest`,
//!   `GetTypesWithActiveOrdersInRegionRequest`, `GetPriceHistoryRequest`);
//! - `eve.market.orders.buyorder.*` (`GetAllPublicOpenInRegionRequest`);
//! - `eve.market.orders.sellorder.*` (`GetAllPublicOpenInRegionRequest`);
//! - `eve.market.price.*` (`GetForAllTypesRequest`);
//! - `eve.market.price.history.*` (`GetForRegionAndTypeRequest`);
//! - `eve.market.region.*` (`GetPriceHistoryForTypeRequest`);
//! - `eve.market.character.sellorder.*` (`GetRequest`);
//! - `eve.market.corporation.sellorder.*` (`GetRequest`);
//! - `eve_public.market.entry.*` (`NewEntryNotice`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_UNAUTHORIZED,
};
use eve_db::{MarketOrderRecord, WorldRepository};
use eve_proto::pb;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
};
use eve_sde::SdeRegistry;
use std::collections::BTreeSet;
use std::sync::Arc;

fn set_isk_currency(b: pb::ProtoBuilder, isk_cents: i64) -> pb::ProtoBuilder {
    let clamped = isk_cents.max(0);
    b.set("units", clamped / 100)
        .set("nanos", ((clamped % 100) * 10_000_000) as i32)
}

fn build_isk_currency(isk_cents: i64) -> Vec<u8> {
    let clamped = isk_cents.max(0);
    pb::message("eve.isk.Currency")
        .set("units", clamped / 100)
        .set("nanos", ((clamped % 100) * 10_000_000) as i32)
        .must_encode()
}

fn encode_order_attributes(
    snap: &eve_db::WorldStateSnapshot,
    order: &MarketOrderRecord,
    is_buy: bool,
    include_private_details: bool,
) -> Vec<u8> {
    let msg_name = if is_buy {
        "eve.market.orders.buyorder.Attributes"
    } else {
        "eve.market.orders.sellorder.Attributes"
    };
    let (days, issued_unix) = snap
        .market_order_meta
        .get(&order.order_id)
        .map(|m| (m.duration_days, m.issued_unix as i64))
        .unwrap_or((90, 1_760_000_000));
    pb::message(msg_name)
        .when(include_private_details, |b| {
            if order.is_npc {
                b.set("issuer_npc_corporation.sequential", order.owner_id as u32)
            } else {
                b.set("issuer_character.sequential", order.owner_id as u32)
            }
        })
        .set("item_type.sequential", order.type_id as u64)
        .set("quantity", order.vol_entered as u32)
        .set_raw_message("price", build_isk_currency(order.price.0))
        .set("days", days)
        .set("issued_date.seconds", issued_unix)
        .set("quantity_minimum", order.min_volume as u32)
        .set("quantity_remain", order.vol_remaining as u32)
        .set("quantity_total", order.vol_entered as u32)
        .set_with("location", |loc| {
            let loc = loc.set("region.sequential", order.region_id as u64);
            if snap.structures.contains_key(&order.station_id) || order.station_id >= 1_000_000_000 {
                loc.set("structure.sequential", order.station_id as u64)
            } else {
                loc.set("station.sequential", order.station_id as u64)
            }
        })
        .set("scope.region", true)
        .when(is_buy && include_private_details, |b| {
            b.set_raw_message("escrow", build_isk_currency(order.escrow.0))
        })
        .must_encode()
}

fn populate_sell_order_entry(
    b: pb::ProtoBuilder,
    snap: &eve_db::WorldStateSnapshot,
    order: &MarketOrderRecord,
    include_private_details: bool,
) -> pb::ProtoBuilder {
    let attrs = encode_order_attributes(snap, order, false, include_private_details);
    b.set("id.sequential", order.order_id as u64)
        .set("attributes", pb::raw_message(attrs))
}

fn populate_buy_order_entry(
    b: pb::ProtoBuilder,
    snap: &eve_db::WorldStateSnapshot,
    order: &MarketOrderRecord,
    include_private_details: bool,
) -> pb::ProtoBuilder {
    let attrs = encode_order_attributes(snap, order, true, include_private_details);
    b.set("id.sequential", order.order_id as u64)
        .set("attributes", pb::raw_message(attrs))
}

fn populate_composite_order_entry(
    b: pb::ProtoBuilder,
    snap: &eve_db::WorldStateSnapshot,
    order: &MarketOrderRecord,
    include_private_details: bool,
) -> pb::ProtoBuilder {
    if order.is_bid {
        b.set_with("buy", |e| {
            populate_buy_order_entry(e, snap, order, include_private_details)
        })
    } else {
        b.set_with("sell", |e| {
            populate_sell_order_entry(e, snap, order, include_private_details)
        })
    }
}

fn can_caller_view_market_location(
    snap: &eve_db::WorldStateSnapshot,
    caller_char_id: i64,
    location_id: i64,
) -> bool {
    crate::domain::market_acl::can_caller_view_market_location(snap, caller_char_id, location_id)
}

fn extract_u64_from_field(dyn_msg: &DynamicQuasarMessage, field_num: u32) -> u64 {
    match dyn_msg.get(field_num) {
        Some(DynamicValue::Message(sub)) => match sub.get(1) {
            Some(DynamicValue::Uint64(n)) => *n,
            Some(DynamicValue::Uint32(n)) => *n as u64,
            Some(DynamicValue::Int64(n)) => *n as u64,
            Some(DynamicValue::Int32(n)) => *n as u64,
            _ => 0,
        },
        Some(DynamicValue::Uint64(n)) => *n,
        Some(DynamicValue::Uint32(n)) => *n as u64,
        Some(DynamicValue::Int64(n)) => *n as u64,
        Some(DynamicValue::Int32(n)) => *n as u64,
        _ => 0,
    }
}

pub fn register_market_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    _sde: Arc<SdeRegistry>,
    _descriptors: Arc<QuasarDescriptorRegistry>,
) {
    // 1. eve.market.orders.GetOrdersRequest -> eve.market.orders.GetOrdersResponse
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.GetOrdersRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let caller_cid = ctx.effective_character_id().unwrap_or(0) as i64;
            let region_id = extract_u64_from_field(dyn_msg, 1) as u32;
            let station_id = extract_u64_from_field(dyn_msg, 2) as i64;
            let item_type = extract_u64_from_field(dyn_msg, 3) as u32;

            let out = db_clone.with_snapshot(|snap| {
                let mut msg = pb::message("eve.market.orders.GetOrdersResponse");
                for o in snap.market_orders.values() {
                    if (region_id == 0 || o.region_id == region_id)
                        && (station_id == 0 || o.station_id == station_id)
                        && (item_type == 0 || o.type_id == item_type)
                        && o.vol_remaining > 0
                        && can_caller_view_market_location(snap, caller_cid, o.station_id)
                    {
                        if o.is_bid {
                            msg = msg.add_with("buy_orders", |e| {
                                populate_buy_order_entry(e, snap, o, false)
                            });
                        } else {
                            msg = msg.add_with("sell_orders", |e| {
                                populate_sell_order_entry(e, snap, o, false)
                            });
                        }
                    }
                }
                msg.must_to_any()
            });

            Ok(out)
        }),
    );

    // 2. eve.market.orders.GetPublicOpenInRegionRequest -> eve.market.orders.GetPublicOpenInRegionResponse
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.GetPublicOpenInRegionRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let caller_cid = ctx.effective_character_id().unwrap_or(0) as i64;
            let region_id = extract_u64_from_field(dyn_msg, 1) as u32;
            let no_type_filter = matches!(dyn_msg.get(2), Some(DynamicValue::Bool(true)));
            let item_type = if no_type_filter {
                0
            } else {
                extract_u64_from_field(dyn_msg, 3) as u32
            };

            let out = db_clone.with_snapshot(|snap| {
                let mut msg = pb::message("eve.market.orders.GetPublicOpenInRegionResponse");
                for o in snap.market_orders.values() {
                    if (region_id == 0 || o.region_id == region_id)
                        && (item_type == 0 || o.type_id == item_type)
                        && o.vol_remaining > 0
                        && can_caller_view_market_location(snap, caller_cid, o.station_id)
                    {
                        msg = msg.add_with("orders", |e| {
                            populate_composite_order_entry(e, snap, o, false)
                        });
                    }
                }
                msg.must_to_any()
            });

            Ok(out)
        }),
    );

    // 3. eve.market.orders.buyorder.GetAllPublicOpenInRegionRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.buyorder.GetAllPublicOpenInRegionRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let caller_cid = ctx.effective_character_id().unwrap_or(0) as i64;
            let region_id = extract_u64_from_field(dyn_msg, 1) as u32;
            let no_type_filter = matches!(dyn_msg.get(2), Some(DynamicValue::Bool(true)));
            let item_type = if no_type_filter {
                0
            } else {
                extract_u64_from_field(dyn_msg, 3) as u32
            };

            let out = db_clone.with_snapshot(|snap| {
                let mut msg = pb::message("eve.market.orders.buyorder.GetAllPublicOpenInRegionResponse");
                for o in snap.market_orders.values() {
                    if o.is_bid
                        && (region_id == 0 || o.region_id == region_id)
                        && (item_type == 0 || o.type_id == item_type)
                        && o.vol_remaining > 0
                        && can_caller_view_market_location(snap, caller_cid, o.station_id)
                    {
                        msg = msg.add_with("orders", |e| {
                            populate_buy_order_entry(e, snap, o, false)
                        });
                    }
                }
                msg.must_to_any()
            });

            Ok(out)
        }),
    );

    // 4. eve.market.orders.sellorder.GetAllPublicOpenInRegionRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.sellorder.GetAllPublicOpenInRegionRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let caller_cid = ctx.effective_character_id().unwrap_or(0) as i64;
            let region_id = extract_u64_from_field(dyn_msg, 1) as u32;
            let no_type_filter = matches!(dyn_msg.get(2), Some(DynamicValue::Bool(true)));
            let item_type = if no_type_filter {
                0
            } else {
                extract_u64_from_field(dyn_msg, 3) as u32
            };

            let out = db_clone.with_snapshot(|snap| {
                let mut msg = pb::message("eve.market.orders.sellorder.GetAllPublicOpenInRegionResponse");
                for o in snap.market_orders.values() {
                    if !o.is_bid
                        && (region_id == 0 || o.region_id == region_id)
                        && (item_type == 0 || o.type_id == item_type)
                        && o.vol_remaining > 0
                        && can_caller_view_market_location(snap, caller_cid, o.station_id)
                    {
                        msg = msg.add_with("orders", |e| {
                            populate_sell_order_entry(e, snap, o, false)
                        });
                    }
                }
                msg.must_to_any()
            });

            Ok(out)
        }),
    );

    // 5. eve.market.orders.GetOpenInStationRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.GetOpenInStationRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let caller_cid = ctx.effective_character_id().unwrap_or(0) as i64;
            let station_id = extract_u64_from_field(dyn_msg, 1) as i64;
            let out = db_clone.with_snapshot(|snap| {
                let mut msg = pb::message("eve.market.orders.GetOpenInStationResponse");
                if !can_caller_view_market_location(snap, caller_cid, station_id) {
                    return msg.must_to_any();
                }
                for o in snap.market_orders.values() {
                    if o.station_id == station_id && o.vol_remaining > 0 {
                        msg = msg.add_with("orders", |e| {
                            populate_composite_order_entry(e, snap, o, false)
                        });
                    }
                }
                msg.must_to_any()
            });
            Ok(out)
        }),
    );

    // 6. eve.market.orders.GetOpenInStructureRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.GetOpenInStructureRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let caller_cid = ctx.effective_character_id().unwrap_or(0) as i64;
            if caller_cid <= 0 {
                return Err((
                    STATUS_UNAUTHORIZED,
                    "Active character identity required to query structure market orders".to_string(),
                ));
            }
            let structure_id = extract_u64_from_field(dyn_msg, 1) as i64;
            db_clone.with_snapshot(|snap| -> Result<GoogleAny, (u32, String)> {
                crate::domain::market_acl::verify_structure_market_query_access(
                    None,
                    snap,
                    caller_cid,
                    structure_id,
                    false,
                )
                .map_err(|e| (STATUS_FORBIDDEN, e.message))?;
                let mut msg = pb::message("eve.market.orders.GetOpenInStructureResponse");
                for o in snap.market_orders.values() {
                    if o.station_id == structure_id && o.vol_remaining > 0 {
                        msg = msg.add_with("orders", |e| {
                            populate_composite_order_entry(e, snap, o, false)
                        });
                    }
                }
                Ok(msg.must_to_any())
            })
        }),
    );

    // 7. eve.market.orders.GetOpenForCharacterRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.GetOpenForCharacterRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let req_cid = extract_u64_from_field(dyn_msg, 1) as i64;
            let effective_cid = ctx.effective_character_id().unwrap_or(0) as i64;
            if effective_cid <= 0 {
                return Err((
                    STATUS_UNAUTHORIZED,
                    "Active character identity required".to_string(),
                ));
            }
            if req_cid != 0 && req_cid != effective_cid {
                return Err((
                    STATUS_FORBIDDEN,
                    format!(
                        "Character {effective_cid} is not authorized to read open market orders for character {req_cid}"
                    ),
                ));
            }
            let target_cid = effective_cid;

            let out = db_clone.with_snapshot(|snap| {
                let mut msg = pb::message("eve.market.orders.GetOpenForCharacterResponse");
                for o in snap.market_orders.values() {
                    if o.owner_id == target_cid && o.vol_remaining > 0 {
                        msg = msg.add_with("orders", |e| {
                            populate_composite_order_entry(e, snap, o, true)
                        });
                    }
                }
                msg.must_to_any()
            });
            Ok(out)
        }),
    );

    // 8. eve.market.orders.GetHistoryForCharacterRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.GetHistoryForCharacterRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let req_cid = extract_u64_from_field(dyn_msg, 1) as i64;
            let effective_cid = ctx.effective_character_id().unwrap_or(0) as i64;
            if effective_cid <= 0 {
                return Err((
                    STATUS_UNAUTHORIZED,
                    "Active character identity required".to_string(),
                ));
            }
            if req_cid != 0 && req_cid != effective_cid {
                return Err((
                    STATUS_FORBIDDEN,
                    format!(
                        "Character {effective_cid} is not authorized to read market order history for character {req_cid}"
                    ),
                ));
            }
            let target_cid = effective_cid;

            let out = db_clone.with_snapshot(|snap| {
                let mut msg = pb::message("eve.market.orders.GetHistoryForCharacterResponse");
                for o in snap.market_orders.values() {
                    if o.owner_id == target_cid {
                        msg = msg.add_with("orders", |e| {
                            populate_composite_order_entry(e, snap, o, true)
                        });
                    }
                }
                msg.must_to_any()
            });
            Ok(out)
        }),
    );

    // 9. eve.market.orders.GetOpenForCorporationRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.GetOpenForCorporationRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let corp_id = extract_u64_from_field(dyn_msg, 1) as i64;
            db_clone.with_snapshot(|snap| -> Result<GoogleAny, (u32, String)> {
                let target_corp = authorize_corp_financial_access(ctx, snap, corp_id)?;
                let mut msg = pb::message("eve.market.orders.GetOpenForCorporationResponse");
                for o in snap.market_orders.values() {
                    if o.owner_id == target_corp && o.vol_remaining > 0 {
                        msg = msg.add_with("orders", |e| {
                            populate_composite_order_entry(e, snap, o, true)
                        });
                    }
                }
                Ok(msg.must_to_any())
            })
        }),
    );

    // 10. eve.market.orders.GetHistoryForCorporationRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.GetHistoryForCorporationRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let corp_id = extract_u64_from_field(dyn_msg, 1) as i64;
            db_clone.with_snapshot(|snap| -> Result<GoogleAny, (u32, String)> {
                let target_corp = authorize_corp_financial_access(ctx, snap, corp_id)?;
                let mut msg = pb::message("eve.market.orders.GetHistoryForCorporationResponse");
                for o in snap.market_orders.values() {
                    if o.owner_id == target_corp {
                        msg = msg.add_with("orders", |e| {
                            populate_composite_order_entry(e, snap, o, true)
                        });
                    }
                }
                Ok(msg.must_to_any())
            })
        }),
    );

    // 11. eve.market.orders.GetTypesWithActiveOrdersInRegionRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.GetTypesWithActiveOrdersInRegionRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let caller_cid = ctx.effective_character_id().unwrap_or(0) as i64;
            let region_id = extract_u64_from_field(dyn_msg, 1) as u32;
            let types = db_clone.with_snapshot(|snap| {
                snap.market_orders
                    .values()
                    .filter(|o| {
                        (region_id == 0 || o.region_id == region_id)
                            && o.vol_remaining > 0
                            && can_caller_view_market_location(snap, caller_cid, o.station_id)
                    })
                    .map(|o| o.type_id)
                    .collect::<BTreeSet<_>>()
            });
            let mut msg = pb::message("eve.market.orders.GetTypesWithActiveOrdersInRegionResponse");
            for tid in types {
                msg = msg.add_with("types", |t| t.set("sequential", tid as u64));
            }
            Ok(msg.must_to_any())
        }),
    );

    // 12. eve.market.orders.GetPriceHistoryRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.GetPriceHistoryRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let caller_cid = ctx.effective_character_id().unwrap_or(0) as i64;
            let region_id = extract_u64_from_field(dyn_msg, 1) as u32;
            let item_type = extract_u64_from_field(dyn_msg, 3) as u32;

            let (avg_cents, count) = db_clone.with_snapshot(|snap| {
                let matching = snap
                    .market_orders
                    .values()
                    .filter(|o| {
                        (region_id == 0 || o.region_id == region_id)
                            && (item_type == 0 || o.type_id == item_type)
                            && can_caller_view_market_location(snap, caller_cid, o.station_id)
                    })
                    .collect::<Vec<_>>();
                if matching.is_empty() {
                    (100_000i64, 0u64)
                } else {
                    let total: i64 = matching.iter().map(|o| o.price.0).sum();
                    (total / matching.len() as i64, matching.len() as u64)
                }
            });

            Ok(pb::message("eve.market.orders.GetPriceHistoryResponse")
                .add_with("entries", |entry| {
                    entry
                        .set("date.seconds", 1_760_000_000i64)
                        .set("order_count", count as u32)
                        .set_with("average", |c| set_isk_currency(c, avg_cents))
                })
                .must_to_any())
        }),
    );

    // 13. eve.market.price.history.GetForRegionAndTypeRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.price.history.GetForRegionAndTypeRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let caller_cid = ctx.effective_character_id().unwrap_or(0) as i64;
            let region_id = extract_u64_from_field(dyn_msg, 1) as u32;
            let item_type = extract_u64_from_field(dyn_msg, 2) as u32;

            let avg_cents = avg_price_cents_in_region(&db_clone, caller_cid, region_id, item_type);

            Ok(
                pb::message("eve.market.price.history.GetForRegionAndTypeResponse")
                    .add_with("day_statistics", |stat| {
                        stat.set("date.seconds", 1_760_000_000i64)
                            .set_with("average", |c| set_isk_currency(c, avg_cents))
                    })
                    .must_to_any(),
            )
        }),
    );

    // 14. eve.market.region.GetPriceHistoryForTypeRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.region.GetPriceHistoryForTypeRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let caller_cid = ctx.effective_character_id().unwrap_or(0) as i64;
            let region_id = extract_u64_from_field(dyn_msg, 1) as u32;
            let item_type = extract_u64_from_field(dyn_msg, 2) as u32;

            let avg_cents = avg_price_cents_in_region(&db_clone, caller_cid, region_id, item_type);

            Ok(
                pb::message("eve.market.region.GetPriceHistoryForTypeResponse")
                    .add_with("day_statistics", |stat| {
                        stat.set("date.seconds", 1_760_000_000i64)
                            .set_with("average", |c| set_isk_currency(c, avg_cents))
                    })
                    .must_to_any(),
            )
        }),
    );

    // 15. eve.market.price.GetForAllTypesRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.price.GetForAllTypesRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let caller_cid = ctx.effective_character_id().unwrap_or(0) as i64;
            let active_types = db_clone.with_snapshot(|snap| {
                snap.market_orders
                    .values()
                    .filter(|o| can_caller_view_market_location(snap, caller_cid, o.station_id))
                    .map(|o| (o.type_id, o.price.0))
                    .collect::<Vec<_>>()
            });

            let mut msg = pb::message("eve.market.price.GetForAllTypesResponse");
            for (tid, cents) in active_types.iter().take(200) {
                let currency = build_isk_currency(*cents);
                msg = msg.add_with("types", |tp| {
                    tp.set("id.sequential", *tid as u64)
                        .set("average_price", pb::raw_message(&currency))
                        .set("adjusted_price", pb::raw_message(&currency))
                });
            }

            Ok(msg.must_to_any())
        }),
    );

    // 16. eve.market.character.sellorder.GetRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.character.sellorder.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let effective_cid = ctx.effective_character_id().unwrap_or(0) as i64;
            if effective_cid <= 0 {
                return Err((
                    STATUS_UNAUTHORIZED,
                    "Active character identity required".to_string(),
                ));
            }
            let order_id = extract_u64_from_field(dyn_msg, 1) as i64;
            db_clone.with_snapshot(|snap| -> Result<GoogleAny, (u32, String)> {
                let order = snap
                    .market_orders
                    .get(&order_id)
                    .ok_or((STATUS_NOT_FOUND, "Order not found".to_string()))?;
                if order.owner_id != effective_cid {
                    return Err((
                        STATUS_FORBIDDEN,
                        format!(
                            "Character {effective_cid} is not authorized to read character sell order {order_id}"
                        ),
                    ));
                }
                Ok(pb::message("eve.market.character.sellorder.GetResponse")
                    .set("order.character.sequential", order.owner_id as u32)
                    .set("order.item.item_type.sequential", order.type_id as u64)
                    .set("order.item.quantity", order.vol_remaining as u32)
                    .must_to_any())
            })
        }),
    );

    // 17. eve.market.corporation.sellorder.GetRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.corporation.sellorder.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let order_id = extract_u64_from_field(dyn_msg, 1) as i64;
            db_clone.with_snapshot(|snap| -> Result<GoogleAny, (u32, String)> {
                let ord = snap
                    .market_orders
                    .get(&order_id)
                    .ok_or((STATUS_NOT_FOUND, "Corp order not found".to_string()))?;
                authorize_corp_financial_access(ctx, snap, ord.owner_id)?;
                Ok(pb::message("eve.market.corporation.sellorder.GetResponse")
                    .set("order.corporation.sequential", ord.owner_id as u32)
                    .set("order.item.item_type.sequential", ord.type_id as u64)
                    .set("order.item.quantity", ord.vol_remaining as u32)
                    .must_to_any())
            })
        }),
    );
}

fn avg_price_cents_in_region(
    db: &WorldRepository,
    caller_cid: i64,
    region_id: u32,
    item_type: u32,
) -> i64 {
    db.with_snapshot(|snap| {
        let matching = snap
            .market_orders
            .values()
            .filter(|o| {
                (region_id == 0 || o.region_id == region_id)
                    && (item_type == 0 || o.type_id == item_type)
                    && can_caller_view_market_location(snap, caller_cid, o.station_id)
            })
            .collect::<Vec<_>>();
        if matching.is_empty() {
            100_000
        } else {
            let total: i64 = matching.iter().map(|o| o.price.0).sum();
            total / matching.len() as i64
        }
    })
}

fn resolve_caller_corp_id(
    ctx: &AuthoritativeContext,
    snap: &eve_db::WorldStateSnapshot,
    effective_cid: i64,
) -> i64 {
    let ctx_corp = ctx
        .effective_corporation_id()
        .map(|id| id as i64)
        .filter(|&id| id > 0);
    let char_corp = snap
        .characters
        .get(&effective_cid)
        .map(|c| c.corporation_id)
        .filter(|&id| id > 0);
    match (ctx_corp, char_corp) {
        (Some(ctx_c), Some(1_000_009)) => ctx_c,
        (_, Some(ch_c)) => ch_c,
        (Some(ctx_c), _) => ctx_c,
        _ => 0,
    }
}

fn authorize_corp_financial_access(
    ctx: &AuthoritativeContext,
    snap: &eve_db::WorldStateSnapshot,
    requested_corp_id: i64,
) -> Result<i64, (u32, String)> {
    let effective_cid = ctx.effective_character_id().unwrap_or(0) as i64;
    if effective_cid <= 0 {
        return Err((
            STATUS_UNAUTHORIZED,
            "Active character identity required".to_string(),
        ));
    }
    let caller_corp = resolve_caller_corp_id(ctx, snap, effective_cid);
    let target_corp = if requested_corp_id != 0 {
        requested_corp_id
    } else {
        caller_corp
    };
    if target_corp <= 0 {
        return Err((
            STATUS_FORBIDDEN,
            "No target corporation available for financial query".to_string(),
        ));
    }
    let is_member = caller_corp == target_corp
        || snap
            .corporations
            .get(&target_corp)
            .is_some_and(|corp| corp.ceo_id == effective_cid || corp.members.contains(&effective_cid));
    if !is_member {
        return Err((
            STATUS_FORBIDDEN,
            format!("Character {effective_cid} is not a member of corporation {target_corp}"),
        ));
    }
    let allowed_roles = eve_db::corp_roles::ROLE_DIRECTOR
        | eve_db::corp_roles::ROLE_ACCOUNTANT
        | eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT
        | eve_db::corp_roles::ROLE_TRADER;
    let ctx_roles = ctx
        .identity
        .as_ref()
        .and_then(|id| id.roles.as_ref().map(|r| r.bitmask))
        .unwrap_or(0);
    let session_roles = if caller_corp == target_corp { ctx_roles } else { 0 };
    let (is_ceo, eff_roles) =
        snap.effective_corp_roles_at(target_corp, effective_cid, None, session_roles, 0);
    let roles = eff_roles | session_roles;
    if is_ceo {
        return Ok(target_corp);
    }
    if (roles & allowed_roles) == 0 {
        return Err((
            STATUS_FORBIDDEN,
            format!(
                "Character {effective_cid} lacks corporate financial/trader role for corporation {target_corp}"
            ),
        ));
    }
    Ok(target_corp)
}

/// Helper for broadcasting a `NewEntryNotice` over Quasar Gateway (`eve_public.market.entry.NewEntryNotice`).
pub fn publish_market_new_entry_notice(
    gateway: &PublicGatewayRouter,
    solar_system_id: u32,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let payload = pb::message("eve_public.market.entry.NewEntryNotice").must_to_any();
    let (_, delivered) =
        gateway.publish_notice(NoticeScope::SolarSystem(solar_system_id), payload, now_unix)?;
    Ok(delivered)
}

#[cfg(test)]
#[path = "quasar_market_tests.rs"]
mod quasar_market_tests;

#[cfg(test)]
#[path = "quasar_market_edges_tests.rs"]
mod quasar_market_edges_tests;

