//! `quasar_inventory_asset` — Quasar V24.01 Inventory, Categories, Groups, Types, & Asset Services (`Q-6`).
//!
//! Implements backend handlers for:
//! - `eve.inventory.category.*` (`GetRequest`, `GetAllRequest`);
//! - `eve.inventory.group.*` (`GetRequest`, `GetAllRequest`, `GetGroupsInCategoryRequest`);
//! - `eve.inventory.market_group.*` (`GetAllRequest`, `GetRequest`);
//! - `eve.inventory.genericitemtype.*` (`GetRequest`, `GetAllRequest`, `GetTypesInGroupRequest`,
//!   `GetTypeInfoByMarketGroupRequest`, `TypeValuationRequest`);
//! - `eve.inventory.genericitem.*` (`SearchRequest`);
//! - `eve.inventory.genericname.*` (`GetNameByIDRequest`, `GetNamesAndCategoriesByIDsRequest`);
//! - `eve.inventory.assetholding.operation.api.*` (`HoldItemsRequest`, `HoldItemsOfTypeRequest`, `SpawnItemsOfTypeRequest`);
//! - `eve.asset.*` (`GetAssetsItemsRequest`, `GetShipAssetsRequest`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{PublicGatewayRouter, STATUS_NOT_FOUND};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::{CategoryId, GroupId, SdeRegistry, TypeId};
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

pub fn register_inventory_asset_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
    _descriptors: Arc<QuasarDescriptorRegistry>,
) {
    // 1. eve.inventory.category.GetRequest -> eve.inventory.category.GetResponse
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.category.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let cat_id = extract_u64_from_field(dyn_msg, 1) as CategoryId;
            let cat = sde_clone
                .categories
                .get(&cat_id)
                .ok_or((STATUS_NOT_FOUND, "Category not found".to_string()))?;

            let mut attrs = Vec::new();
            encode_string_field(1, &cat.name, &mut attrs);
            encode_varint_field(2, if cat.published { 1 } else { 0 }, &mut attrs);

            let mut out = Vec::new();
            encode_bytes_field(1, &attrs, &mut out);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.category.GetResponse"),
                value: out,
            })
        }),
    );

    // 2. eve.inventory.category.GetAllRequest -> eve.inventory.category.GetAllResponse
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.category.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            for cid in sde_clone.categories.keys() {
                encode_identifier(1, *cid as u64, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.category.GetAllResponse"),
                value: out,
            })
        }),
    );

    // 3. eve.inventory.group.GetRequest -> eve.inventory.group.GetResponse
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.group.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let gid = extract_u64_from_field(dyn_msg, 1) as GroupId;
            let grp = sde_clone
                .groups
                .get(&gid)
                .ok_or((STATUS_NOT_FOUND, "Group not found".to_string()))?;

            let mut attrs = Vec::new();
            encode_string_field(1, &grp.name, &mut attrs);
            encode_identifier(2, grp.category_id as u64, &mut attrs);
            encode_varint_field(3, if grp.published { 1 } else { 0 }, &mut attrs);

            let mut out = Vec::new();
            encode_bytes_field(2, &attrs, &mut out);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.group.GetResponse"),
                value: out,
            })
        }),
    );

    // 4. eve.inventory.group.GetAllRequest -> eve.inventory.group.GetAllResponse
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.group.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            for gid in sde_clone.groups.keys().take(500) {
                encode_identifier(1, *gid as u64, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.group.GetAllResponse"),
                value: out,
            })
        }),
    );

    // 5. eve.inventory.group.GetGroupsInCategoryRequest -> eve.inventory.group.GetGroupsInCategoryResponse
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.group.GetGroupsInCategoryRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let cat_id = extract_u64_from_field(dyn_msg, 1) as CategoryId;
            let mut out = Vec::new();
            for (gid, grp) in &sde_clone.groups {
                if grp.category_id == cat_id {
                    encode_identifier(1, *gid as u64, &mut out);
                }
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.group.GetGroupsInCategoryResponse"
                ),
                value: out,
            })
        }),
    );

    // 6. eve.inventory.market_group.GetAllRequest -> eve.inventory.market_group.GetAllResponse
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.market_group.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            for mgid in sde_clone.market_groups.keys() {
                encode_identifier(1, *mgid as u64, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.market_group.GetAllResponse"
                ),
                value: out,
            })
        }),
    );

    // 7. eve.inventory.market_group.GetRequest -> eve.inventory.market_group.GetResponse
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.market_group.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let mg_id = extract_u64_from_field(dyn_msg, 1) as u32;
            let mg = sde_clone
                .market_groups
                .get(&mg_id)
                .ok_or((STATUS_NOT_FOUND, "Market group not found".to_string()))?;

            let mut attrs = Vec::new();
            match mg.parent_group_id {
                Some(pid) => {
                    encode_varint_field(1, 0, &mut attrs);
                    encode_identifier(2, pid as u64, &mut attrs);
                }
                None => {
                    encode_varint_field(1, 1, &mut attrs);
                }
            }
            encode_string_field(3, &mg.name, &mut attrs);
            encode_string_field(4, "", &mut attrs);

            let mut out = Vec::new();
            encode_bytes_field(1, &attrs, &mut out);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.market_group.GetResponse"),
                value: out,
            })
        }),
    );

    // 8. eve.inventory.genericitemtype.GetRequest -> eve.inventory.genericitemtype.GetResponse
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.genericitemtype.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let tid = extract_u64_from_field(dyn_msg, 1) as TypeId;
            let t = sde_clone
                .types
                .get(&tid)
                .ok_or((STATUS_NOT_FOUND, "Item type not found".to_string()))?;

            let mut attrs = Vec::new();
            encode_string_field(2, &t.name, &mut attrs);
            encode_string_field(3, &t.description, &mut attrs);
            encode_identifier(4, t.group_id as u64, &mut attrs);

            let mut out = Vec::new();
            encode_bytes_field(1, &attrs, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.genericitemtype.GetResponse"
                ),
                value: out,
            })
        }),
    );

    // 9. eve.inventory.genericitemtype.GetAllRequest -> eve.inventory.genericitemtype.GetAllResponse
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.genericitemtype.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            for tid in sde_clone.types.keys().take(500) {
                encode_identifier(1, *tid as u64, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.genericitemtype.GetAllResponse"
                ),
                value: out,
            })
        }),
    );

    // 10. eve.inventory.genericitemtype.GetTypesInGroupRequest -> eve.inventory.genericitemtype.GetTypesInGroupResponse
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.genericitemtype.GetTypesInGroupRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let gid = extract_u64_from_field(dyn_msg, 1) as GroupId;
            let mut out = Vec::new();
            for (tid, t) in &sde_clone.types {
                if t.group_id == gid {
                    encode_identifier(1, *tid as u64, &mut out);
                }
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.genericitemtype.GetTypesInGroupResponse"
                ),
                value: out,
            })
        }),
    );

    // 11. eve.inventory.genericitemtype.GetTypeInfoByMarketGroupRequest
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.genericitemtype.GetTypeInfoByMarketGroupRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let mg_id = extract_u64_from_field(dyn_msg, 4) as u32;
            let type_ids = sde_clone.types_in_market_group(mg_id);

            let mut out = Vec::new();
            for tid in type_ids {
                if let Some(t) = sde_clone.types.get(&tid) {
                    let mut ti = Vec::new();
                    encode_identifier(1, tid as u64, &mut ti);
                    encode_string_field(2, &t.name, &mut ti);
                    encode_bytes_field(1, &ti, &mut out);
                }
            }

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.genericitemtype.GetTypeInfoByMarketGroupResponse"
                ),
                value: out,
            })
        }),
    );

    // 12. eve.inventory.genericitemtype.TypeValuationRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.inventory.genericitemtype.TypeValuationRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let tid = extract_u64_from_field(dyn_msg, 1) as TypeId;
            let cents = db_clone.with_snapshot(|snap| {
                snap.market_orders
                    .values()
                    .find(|o| o.type_id == tid)
                    .map(|o| o.price.0)
                    .unwrap_or(500_000)
            });

            let mut out = Vec::new();
            encode_currency(1, cents, &mut out);
            let ts = GoogleTimestamp {
                seconds: 1_760_000_000,
                nanos: 0,
            };
            encode_bytes_field(2, &ts.encode_proto(), &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.genericitemtype.TypeValuationResponse"
                ),
                value: out,
            })
        }),
    );

    // 13. eve.inventory.genericitem.SearchRequest
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.genericitem.SearchRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let query = match dyn_msg.get(1) {
                Some(DynamicValue::Message(sub)) => match sub.get(1) {
                    Some(DynamicValue::String(s)) => s.to_lowercase(),
                    _ => String::new(),
                },
                Some(DynamicValue::String(s)) => s.to_lowercase(),
                _ => String::new(),
            };

            let mut out = Vec::new();
            for (tid, t) in &sde_clone.types {
                if query.is_empty() || t.name.to_lowercase().contains(&query) {
                    encode_identifier(1, *tid as u64, &mut out);
                    if out.len() >= 200 {
                        break;
                    }
                }
            }

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.genericitem.SearchResponse"
                ),
                value: out,
            })
        }),
    );

    // 14. eve.inventory.genericname.GetNameByIDRequest
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.genericname.GetNameByIDRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let id = extract_u64_from_field(dyn_msg, 1) as TypeId;
            let name = sde_clone
                .types
                .get(&id)
                .map(|t| t.name.clone())
                .unwrap_or_else(|| format!("Item #{id}"));

            let mut out = Vec::new();
            encode_string_field(1, &name, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.genericname.GetNameByIDResponse"
                ),
                value: out,
            })
        }),
    );

    // 15. eve.inventory.genericname.GetNamesAndCategoriesByIDsRequest
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.genericname.GetNamesAndCategoriesByIDsRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let mut out = Vec::new();
            if let Some(DynamicValue::Repeated(items)) = dyn_msg.get(1) {
                for item in items {
                    let id = match item {
                        DynamicValue::Message(sub) => match sub.get(1) {
                            Some(DynamicValue::Uint64(n)) => *n as TypeId,
                            Some(DynamicValue::Uint32(n)) => *n as TypeId,
                            _ => 0,
                        },
                        DynamicValue::Uint64(n) => *n as TypeId,
                        DynamicValue::Uint32(n) => *n as TypeId,
                        _ => 0,
                    };
                    if let Some(t) = sde_clone.types.get(&id) {
                        let cat_id = sde_clone
                            .groups
                            .get(&t.group_id)
                            .map(|g| g.category_id)
                            .unwrap_or(0);
                        let mut entry = Vec::new();
                        encode_identifier(1, id as u64, &mut entry);
                        encode_string_field(2, &t.name, &mut entry);
                        encode_identifier(3, cat_id as u64, &mut entry);
                        encode_bytes_field(1, &entry, &mut out);
                    }
                }
            }

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.genericname.GetNamesAndCategoriesByIDsResponse"
                ),
                value: out,
            })
        }),
    );

    // 16. eve.inventory.assetholding.operation.api.HoldItemsRequest
    router.register_handler(
        "eve.inventory.assetholding.operation.api.HoldItemsRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let op_id = extract_u64_from_field(dyn_msg, 1);
            let mut out = Vec::new();
            encode_identifier(1, 999_001, &mut out); // container id
            encode_identifier(2, op_id.max(1), &mut out); // item id
            let mut loc = Vec::new();
            encode_identifier(1, 60_003_760, &mut loc); // solar system / station
            encode_bytes_field(3, &loc, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.assetholding.operation.api.HoldItemsResponse"
                ),
                value: out,
            })
        }),
    );

    // 17. eve.inventory.assetholding.operation.api.HoldItemsOfTypeRequest
    router.register_handler(
        "eve.inventory.assetholding.operation.api.HoldItemsOfTypeRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let op_id = extract_u64_from_field(dyn_msg, 1);
            let mut out = Vec::new();
            encode_identifier(1, 999_002, &mut out);
            encode_identifier(2, op_id.max(1), &mut out);
            let mut loc = Vec::new();
            encode_identifier(1, 60_003_760, &mut loc);
            encode_bytes_field(3, &loc, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.assetholding.operation.api.HoldItemsOfTypeResponse"
                ),
                value: out,
            })
        }),
    );

    // 18. eve.inventory.assetholding.operation.api.SpawnItemsOfTypeRequest
    router.register_handler(
        "eve.inventory.assetholding.operation.api.SpawnItemsOfTypeRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let op_id = extract_u64_from_field(dyn_msg, 1);
            let mut out = Vec::new();
            encode_identifier(1, op_id.max(1), &mut out);
            let mut loc = Vec::new();
            encode_identifier(1, 60_003_760, &mut loc);
            encode_bytes_field(2, &loc, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.assetholding.operation.api.SpawnItemsOfTypeResponse"
                ),
                value: out,
            })
        }),
    );

    // 19. eve.asset.GetAssetsItemsRequest -> eve.asset.GetAssetItemsResponse
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.asset.GetAssetsItemsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let req_cid = extract_u64_from_field(dyn_msg, 1) as i64;
            let effective_cid = ctx.effective_character_id().unwrap_or(0) as i64;
            let target_cid = if req_cid != 0 { req_cid } else { effective_cid };

            let items = db_clone.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|item| item.owner_id == target_cid)
                    .cloned()
                    .collect::<Vec<_>>()
            });

            let mut out = Vec::new();
            for item in items {
                let mut char_asset = Vec::new();
                let mut asset_data = Vec::new();
                encode_identifier(1, item.item_id as u64, &mut asset_data);
                encode_identifier(2, item.type_id as u64, &mut asset_data);
                encode_varint_field(3, item.quantity as u64, &mut asset_data);
                encode_bytes_field(1, &asset_data, &mut char_asset);
                encode_identifier(2, item.location_id as u64, &mut char_asset);
                encode_bytes_field(1, &char_asset, &mut out);
            }

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.asset.GetAssetItemsResponse"),
                value: out,
            })
        }),
    );

    // 20. eve.asset.GetShipAssetsRequest -> eve.asset.GetShipAssetsResponse
    let db_clone = Arc::clone(&db);
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.asset.GetShipAssetsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let req_cid = extract_u64_from_field(dyn_msg, 1) as i64;
            let effective_cid = ctx.effective_character_id().unwrap_or(0) as i64;
            let target_cid = if req_cid != 0 { req_cid } else { effective_cid };

            let ships = db_clone.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|item| {
                        item.owner_id == target_cid
                            && sde_clone
                                .types
                                .get(&item.type_id)
                                .map(|t| {
                                    sde_clone
                                        .groups
                                        .get(&t.group_id)
                                        .map(|g| g.category_id == 6)
                                        .unwrap_or(false)
                                })
                                .unwrap_or(false)
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            });

            let mut out = Vec::new();
            for ship in ships {
                let mut char_ship = Vec::new();
                let mut ship_data = Vec::new();
                encode_identifier(1, ship.item_id as u64, &mut ship_data);
                encode_identifier(2, ship.type_id as u64, &mut ship_data);
                encode_bytes_field(1, &ship_data, &mut char_ship);
                encode_identifier(2, ship.location_id as u64, &mut char_ship);
                encode_bytes_field(1, &char_ship, &mut out);
            }

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.asset.GetShipAssetsResponse"),
                value: out,
            })
        }),
    );
}
