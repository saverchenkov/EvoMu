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
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, AuthoritativeContext, DynamicQuasarMessage,
    DynamicValue, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::collections::BTreeSet;
use std::sync::Arc;

fn encode_identifier(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut id_buf = Vec::new();
    encode_varint_field(1, id, &mut id_buf);
    encode_bytes_field(field_num, &id_buf, out);
}

fn encode_currency(field_num: u32, isk_cents: i64, out: &mut Vec<u8>) {
    let mut cur_buf = Vec::new();
    encode_varint_field(1, (isk_cents.max(0) / 100) as u64, &mut cur_buf);
    encode_bytes_field(field_num, &cur_buf, out);
}

fn encode_timestamp(field_num: u32, secs: i64, out: &mut Vec<u8>) {
    let ts = GoogleTimestamp {
        seconds: secs,
        nanos: 0,
    };
    encode_bytes_field(field_num, &ts.encode_proto(), out);
}

fn encode_order_attributes(order: &MarketOrderRecord, is_buy: bool) -> Vec<u8> {
    let mut out = Vec::new();
    // 1: issuer_character
    encode_identifier(1, order.owner_id as u64, &mut out);
    // 3: item_type
    encode_identifier(3, order.type_id as u64, &mut out);
    // 4: quantity
    encode_varint_field(4, order.vol_entered as u64, &mut out);
    // 5: price
    encode_currency(5, order.price.0, &mut out);
    // 6: days
    encode_varint_field(6, 90, &mut out);
    // 8: issued_date
    encode_timestamp(8, 1_760_000_000, &mut out);
    // 9: quantity_minimum
    encode_varint_field(9, order.min_volume as u64, &mut out);
    // 10: quantity_remain
    encode_varint_field(10, order.vol_remaining as u64, &mut out);
    // 11: quantity_total
    encode_varint_field(11, order.vol_entered as u64, &mut out);
    // 12: location
    encode_identifier(12, order.station_id as u64, &mut out);
    // 13: scope
    encode_identifier(13, order.region_id as u64, &mut out);
    if is_buy {
        // 15: escrow
        encode_currency(15, order.escrow.0, &mut out);
    }
    out
}

fn encode_sell_order_entry(order: &MarketOrderRecord) -> Vec<u8> {
    let mut out = Vec::new();
    encode_identifier(1, order.order_id as u64, &mut out);
    let attrs = encode_order_attributes(order, false);
    encode_bytes_field(2, &attrs, &mut out);
    out
}

fn encode_buy_order_entry(order: &MarketOrderRecord) -> Vec<u8> {
    let mut out = Vec::new();
    encode_identifier(1, order.order_id as u64, &mut out);
    let attrs = encode_order_attributes(order, true);
    encode_bytes_field(2, &attrs, &mut out);
    out
}

fn encode_composite_order_entry(order: &MarketOrderRecord) -> Vec<u8> {
    let mut out = Vec::new();
    if order.is_bid {
        let buy_entry = encode_buy_order_entry(order);
        encode_bytes_field(1, &buy_entry, &mut out);
    } else {
        let sell_entry = encode_sell_order_entry(order);
        encode_bytes_field(2, &sell_entry, &mut out);
    }
    out
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
        Arc::new(move |_ctx, _any, dyn_msg| {
            let region_id = extract_u64_from_field(dyn_msg, 1) as u32;
            let station_id = extract_u64_from_field(dyn_msg, 2) as i64;
            let item_type = extract_u64_from_field(dyn_msg, 3) as u32;

            let orders = db_clone.with_snapshot(|snap| {
                snap.market_orders
                    .values()
                    .filter(|o| {
                        (region_id == 0 || o.region_id == region_id)
                            && (station_id == 0 || o.station_id == station_id)
                            && (item_type == 0 || o.type_id == item_type)
                            && o.vol_remaining > 0
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            });

            let mut out = Vec::new();
            for o in &orders {
                if o.is_bid {
                    let entry = encode_buy_order_entry(o);
                    encode_bytes_field(2, &entry, &mut out);
                } else {
                    let entry = encode_sell_order_entry(o);
                    encode_bytes_field(1, &entry, &mut out);
                }
            }

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.market.orders.GetOrdersResponse"),
                value: out,
            })
        }),
    );

    // 2. eve.market.orders.GetPublicOpenInRegionRequest -> eve.market.orders.GetPublicOpenInRegionResponse
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.GetPublicOpenInRegionRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let region_id = extract_u64_from_field(dyn_msg, 1) as u32;
            let no_type_filter = matches!(dyn_msg.get(2), Some(DynamicValue::Bool(true)));
            let item_type = if no_type_filter {
                0
            } else {
                extract_u64_from_field(dyn_msg, 3) as u32
            };

            let orders = db_clone.with_snapshot(|snap| {
                snap.market_orders
                    .values()
                    .filter(|o| {
                        (region_id == 0 || o.region_id == region_id)
                            && (item_type == 0 || o.type_id == item_type)
                            && o.vol_remaining > 0
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            });

            let mut out = Vec::new();
            for o in orders {
                let comp = encode_composite_order_entry(&o);
                encode_bytes_field(1, &comp, &mut out);
            }

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.market.orders.GetPublicOpenInRegionResponse"
                ),
                value: out,
            })
        }),
    );

    // 3. eve.market.orders.buyorder.GetAllPublicOpenInRegionRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.buyorder.GetAllPublicOpenInRegionRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let region_id = extract_u64_from_field(dyn_msg, 1) as u32;
            let no_type_filter = matches!(dyn_msg.get(2), Some(DynamicValue::Bool(true)));
            let item_type = if no_type_filter {
                0
            } else {
                extract_u64_from_field(dyn_msg, 3) as u32
            };

            let orders = db_clone.with_snapshot(|snap| {
                snap.market_orders
                    .values()
                    .filter(|o| {
                        o.is_bid
                            && (region_id == 0 || o.region_id == region_id)
                            && (item_type == 0 || o.type_id == item_type)
                            && o.vol_remaining > 0
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            });

            let mut out = Vec::new();
            for o in orders {
                let entry = encode_buy_order_entry(&o);
                encode_bytes_field(1, &entry, &mut out);
            }

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.market.orders.buyorder.GetAllPublicOpenInRegionResponse"
                ),
                value: out,
            })
        }),
    );

    // 4. eve.market.orders.sellorder.GetAllPublicOpenInRegionRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.sellorder.GetAllPublicOpenInRegionRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let region_id = extract_u64_from_field(dyn_msg, 1) as u32;
            let no_type_filter = matches!(dyn_msg.get(2), Some(DynamicValue::Bool(true)));
            let item_type = if no_type_filter {
                0
            } else {
                extract_u64_from_field(dyn_msg, 3) as u32
            };

            let orders = db_clone.with_snapshot(|snap| {
                snap.market_orders
                    .values()
                    .filter(|o| {
                        !o.is_bid
                            && (region_id == 0 || o.region_id == region_id)
                            && (item_type == 0 || o.type_id == item_type)
                            && o.vol_remaining > 0
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            });

            let mut out = Vec::new();
            for o in orders {
                let entry = encode_sell_order_entry(&o);
                encode_bytes_field(1, &entry, &mut out);
            }

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.market.orders.sellorder.GetAllPublicOpenInRegionResponse"
                ),
                value: out,
            })
        }),
    );

    // 5. eve.market.orders.GetOpenInStationRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.GetOpenInStationRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let station_id = extract_u64_from_field(dyn_msg, 1) as i64;
            let orders = db_clone.with_snapshot(|snap| {
                snap.market_orders
                    .values()
                    .filter(|o| o.station_id == station_id && o.vol_remaining > 0)
                    .cloned()
                    .collect::<Vec<_>>()
            });
            let mut out = Vec::new();
            for o in orders {
                let comp = encode_composite_order_entry(&o);
                encode_bytes_field(1, &comp, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.market.orders.GetOpenInStationResponse"
                ),
                value: out,
            })
        }),
    );

    // 6. eve.market.orders.GetOpenInStructureRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.GetOpenInStructureRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let structure_id = extract_u64_from_field(dyn_msg, 1) as i64;
            let orders = db_clone.with_snapshot(|snap| {
                snap.market_orders
                    .values()
                    .filter(|o| o.station_id == structure_id && o.vol_remaining > 0)
                    .cloned()
                    .collect::<Vec<_>>()
            });
            let mut out = Vec::new();
            for o in orders {
                let comp = encode_composite_order_entry(&o);
                encode_bytes_field(1, &comp, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.market.orders.GetOpenInStructureResponse"
                ),
                value: out,
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

            let orders = db_clone.with_snapshot(|snap| {
                snap.market_orders
                    .values()
                    .filter(|o| o.owner_id == target_cid && o.vol_remaining > 0)
                    .cloned()
                    .collect::<Vec<_>>()
            });
            let mut out = Vec::new();
            for o in orders {
                let comp = encode_composite_order_entry(&o);
                encode_bytes_field(1, &comp, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.market.orders.GetOpenForCharacterResponse"
                ),
                value: out,
            })
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

            let orders = db_clone.with_snapshot(|snap| {
                snap.market_orders
                    .values()
                    .filter(|o| o.owner_id == target_cid)
                    .cloned()
                    .collect::<Vec<_>>()
            });
            let mut out = Vec::new();
            for o in orders {
                let comp = encode_composite_order_entry(&o);
                encode_bytes_field(1, &comp, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.market.orders.GetHistoryForCharacterResponse"
                ),
                value: out,
            })
        }),
    );

    // 9. eve.market.orders.GetOpenForCorporationRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.GetOpenForCorporationRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let corp_id = extract_u64_from_field(dyn_msg, 1) as i64;
            let orders = db_clone.with_snapshot(|snap| {
                let target_corp = authorize_corp_financial_access(ctx, snap, corp_id)?;
                Ok::<_, (u32, String)>(
                    snap.market_orders
                        .values()
                        .filter(|o| o.owner_id == target_corp && o.vol_remaining > 0)
                        .cloned()
                        .collect::<Vec<_>>(),
                )
            })?;
            let mut out = Vec::new();
            for o in orders {
                let comp = encode_composite_order_entry(&o);
                encode_bytes_field(1, &comp, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.market.orders.GetOpenForCorporationResponse"
                ),
                value: out,
            })
        }),
    );

    // 10. eve.market.orders.GetHistoryForCorporationRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.GetHistoryForCorporationRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let corp_id = extract_u64_from_field(dyn_msg, 1) as i64;
            let orders = db_clone.with_snapshot(|snap| {
                let target_corp = authorize_corp_financial_access(ctx, snap, corp_id)?;
                Ok::<_, (u32, String)>(
                    snap.market_orders
                        .values()
                        .filter(|o| o.owner_id == target_corp)
                        .cloned()
                        .collect::<Vec<_>>(),
                )
            })?;
            let mut out = Vec::new();
            for o in orders {
                let comp = encode_composite_order_entry(&o);
                encode_bytes_field(1, &comp, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.market.orders.GetHistoryForCorporationResponse"
                ),
                value: out,
            })
        }),
    );

    // 11. eve.market.orders.GetTypesWithActiveOrdersInRegionRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.GetTypesWithActiveOrdersInRegionRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let region_id = extract_u64_from_field(dyn_msg, 1) as u32;
            let types = db_clone.with_snapshot(|snap| {
                snap.market_orders
                    .values()
                    .filter(|o| (region_id == 0 || o.region_id == region_id) && o.vol_remaining > 0)
                    .map(|o| o.type_id)
                    .collect::<BTreeSet<_>>()
            });
            let mut out = Vec::new();
            for tid in types {
                encode_identifier(1, tid as u64, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.market.orders.GetTypesWithActiveOrdersInRegionResponse"
                ),
                value: out,
            })
        }),
    );

    // 12. eve.market.orders.GetPriceHistoryRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.orders.GetPriceHistoryRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let region_id = extract_u64_from_field(dyn_msg, 1) as u32;
            let item_type = extract_u64_from_field(dyn_msg, 3) as u32;

            let (avg_cents, count) = db_clone.with_snapshot(|snap| {
                let matching = snap
                    .market_orders
                    .values()
                    .filter(|o| {
                        (region_id == 0 || o.region_id == region_id)
                            && (item_type == 0 || o.type_id == item_type)
                    })
                    .collect::<Vec<_>>();
                if matching.is_empty() {
                    (100_000i64, 0u64)
                } else {
                    let total: i64 = matching.iter().map(|o| o.price.0).sum();
                    (total / matching.len() as i64, matching.len() as u64)
                }
            });

            let mut entry = Vec::new();
            encode_timestamp(1, 1_760_000_000, &mut entry);
            encode_currency(2, avg_cents, &mut entry);
            encode_varint_field(5, count, &mut entry);

            let mut out = Vec::new();
            encode_bytes_field(1, &entry, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.market.orders.GetPriceHistoryResponse"
                ),
                value: out,
            })
        }),
    );

    // 13. eve.market.price.history.GetForRegionAndTypeRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.price.history.GetForRegionAndTypeRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let region_id = extract_u64_from_field(dyn_msg, 1) as u32;
            let item_type = extract_u64_from_field(dyn_msg, 2) as u32;

            let avg_cents = db_clone.with_snapshot(|snap| {
                let matching = snap
                    .market_orders
                    .values()
                    .filter(|o| {
                        (region_id == 0 || o.region_id == region_id)
                            && (item_type == 0 || o.type_id == item_type)
                    })
                    .collect::<Vec<_>>();
                if matching.is_empty() {
                    100_000i64
                } else {
                    let total: i64 = matching.iter().map(|o| o.price.0).sum();
                    total / matching.len() as i64
                }
            });

            let mut day_stat = Vec::new();
            encode_timestamp(1, 1_760_000_000, &mut day_stat);
            encode_currency(2, avg_cents, &mut day_stat);

            let mut out = Vec::new();
            encode_bytes_field(1, &day_stat, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.market.price.history.GetForRegionAndTypeResponse"
                ),
                value: out,
            })
        }),
    );

    // 14. eve.market.region.GetPriceHistoryForTypeRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.region.GetPriceHistoryForTypeRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let region_id = extract_u64_from_field(dyn_msg, 1) as u32;
            let item_type = extract_u64_from_field(dyn_msg, 2) as u32;

            let avg_cents = db_clone.with_snapshot(|snap| {
                let matching = snap
                    .market_orders
                    .values()
                    .filter(|o| {
                        (region_id == 0 || o.region_id == region_id)
                            && (item_type == 0 || o.type_id == item_type)
                    })
                    .collect::<Vec<_>>();
                if matching.is_empty() {
                    100_000i64
                } else {
                    let total: i64 = matching.iter().map(|o| o.price.0).sum();
                    total / matching.len() as i64
                }
            });

            let mut day_stat = Vec::new();
            encode_timestamp(1, 1_760_000_000, &mut day_stat);
            encode_currency(2, avg_cents, &mut day_stat);

            let mut out = Vec::new();
            encode_bytes_field(1, &day_stat, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.market.region.GetPriceHistoryForTypeResponse"
                ),
                value: out,
            })
        }),
    );

    // 15. eve.market.price.GetForAllTypesRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.price.GetForAllTypesRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let active_types = db_clone.with_snapshot(|snap| {
                snap.market_orders
                    .values()
                    .map(|o| (o.type_id, o.price.0))
                    .collect::<Vec<_>>()
            });

            let mut out = Vec::new();
            for (tid, cents) in active_types.iter().take(200) {
                let mut type_price = Vec::new();
                encode_identifier(1, *tid as u64, &mut type_price);
                encode_currency(2, *cents, &mut type_price);
                encode_currency(3, *cents, &mut type_price);
                encode_bytes_field(1, &type_price, &mut out);
            }

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.market.price.GetForAllTypesResponse"
                ),
                value: out,
            })
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
            let order = db_clone
                .with_snapshot(|snap| snap.market_orders.get(&order_id).cloned())
                .ok_or((STATUS_NOT_FOUND, "Order not found".to_string()))?;
            if order.owner_id != effective_cid {
                return Err((
                    STATUS_FORBIDDEN,
                    format!(
                        "Character {effective_cid} is not authorized to read character sell order {order_id}"
                    ),
                ));
            }

            let attrs = encode_order_attributes(&order, false);
            let mut out = Vec::new();
            encode_bytes_field(1, &attrs, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.market.character.sellorder.GetResponse"
                ),
                value: out,
            })
        }),
    );

    // 17. eve.market.corporation.sellorder.GetRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.market.corporation.sellorder.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let order_id = extract_u64_from_field(dyn_msg, 1) as i64;
            let order = db_clone.with_snapshot(|snap| {
                let ord = snap
                    .market_orders
                    .get(&order_id)
                    .cloned()
                    .ok_or((STATUS_NOT_FOUND, "Corp order not found".to_string()))?;
                authorize_corp_financial_access(ctx, snap, ord.owner_id)?;
                Ok::<_, (u32, String)>(ord)
            })?;

            let attrs = encode_order_attributes(&order, false);
            let mut out = Vec::new();
            encode_bytes_field(1, &attrs, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.market.corporation.sellorder.GetResponse"
                ),
                value: out,
            })
        }),
    );
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
    let mut roles = if caller_corp == target_corp { ctx_roles } else { 0 };
    if let Some(corp) = snap.corporations.get(&target_corp) {
        if corp.ceo_id == effective_cid {
            return Ok(target_corp);
        }
        roles |= corp.member_roles.get(&effective_cid).copied().unwrap_or(0);
    }
    if let Some(ra) = snap.corp_role_assignments.get(&(target_corp, effective_cid)) {
        roles |= ra.roles;
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
    let payload = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.market.entry.NewEntryNotice"),
        value: Vec::new(),
    };
    let (_, delivered) =
        gateway.publish_notice(NoticeScope::SolarSystem(solar_system_id), payload, now_unix)?;
    Ok(delivered)
}
