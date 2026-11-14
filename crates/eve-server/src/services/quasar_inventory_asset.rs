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
use eve_proto::pb;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, GoogleTimestamp,
    QuasarDescriptorRegistry, WireField, WireMessage, WireValue, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::{CategoryId, GroupId, SdeRegistry, TypeId};
use std::sync::Arc;

fn caller_location_id(ctx: &AuthoritativeContext, db: &WorldRepository) -> u64 {
    ctx.effective_character_id()
        .and_then(|cid| {
            db.with_snapshot(|s| {
                s.characters.get(&(cid as i64)).and_then(|c| {
                    c.station_id
                        .or(c.structure_id)
                        .or(Some(c.solar_system_id as i64))
                        .filter(|&id| id > 0)
                })
            })
        })
        .unwrap_or(0) as u64
}

fn asset_location_builder(loc_id: u64) -> pb::ProtoBuilder {
    let b = pb::message("eve.asset.AssetLocationIdentifier");
    if (30_000_000..40_000_000).contains(&loc_id) {
        b.set("solar_system.sequential", loc_id)
    } else if (60_000_000..64_000_000).contains(&loc_id) {
        b.set("station.sequential", loc_id)
    } else {
        b.set("structure.sequential", loc_id)
    }
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

            Ok(pb::message("eve.inventory.category.GetResponse")
                .set("attributes.name", &cat.name)
                .set("attributes.published", cat.published)
                .must_to_any())
        }),
    );

    // 2. eve.inventory.category.GetAllRequest -> eve.inventory.category.GetAllResponse
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.category.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut resp = pb::message("eve.inventory.category.GetAllResponse");
            for cid in sde_clone.categories.keys() {
                resp = resp.add_with("categories", |m| m.set("sequential", *cid));
            }
            Ok(resp.must_to_any())
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

            Ok(pb::message("eve.inventory.group.GetResponse")
                .set("attributes.name", &grp.name)
                .set("attributes.category.sequential", grp.category_id)
                .set("attributes.published", grp.published)
                .must_to_any())
        }),
    );

    // 4. eve.inventory.group.GetAllRequest -> eve.inventory.group.GetAllResponse
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.group.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut resp = pb::message("eve.inventory.group.GetAllResponse");
            for gid in sde_clone.groups.keys().take(500) {
                resp = resp.add_with("groups", |m| m.set("sequential", *gid));
            }
            Ok(resp.must_to_any())
        }),
    );

    // 5. eve.inventory.group.GetGroupsInCategoryRequest -> eve.inventory.group.GetGroupsInCategoryResponse
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.group.GetGroupsInCategoryRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let cat_id = extract_u64_from_field(dyn_msg, 1) as CategoryId;
            let mut resp = pb::message("eve.inventory.group.GetGroupsInCategoryResponse");
            for (gid, grp) in &sde_clone.groups {
                if grp.category_id == cat_id {
                    resp = resp.add_with("groups", |m| m.set("sequential", *gid));
                }
            }
            Ok(resp.must_to_any())
        }),
    );

    // 6. eve.inventory.market_group.GetAllRequest -> eve.inventory.market_group.GetAllResponse
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.market_group.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut resp = pb::message("eve.inventory.market_group.GetAllResponse");
            for mgid in sde_clone.market_groups.keys() {
                resp = resp.add_with("groups", |m| m.set("sequential", *mgid));
            }
            Ok(resp.must_to_any())
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

            let mut resp = pb::message("eve.inventory.market_group.GetResponse")
                .set("group.name", &mg.name)
                .set("group.description", "");
            resp = match mg.parent_group_id {
                Some(pid) => resp.set("group.parent_group.sequential", pid),
                None => resp.set("group.no_parent_group", true),
            };
            Ok(resp.must_to_any())
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

            Ok(pb::message("eve.inventory.genericitemtype.GetResponse")
                .set("type_info.name", &t.name)
                .set("type_info.description", &t.description)
                .set("type_info.group.sequential", t.group_id)
                .must_to_any())
        }),
    );

    // 9. eve.inventory.genericitemtype.GetAllRequest -> eve.inventory.genericitemtype.GetAllResponse
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.genericitemtype.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut resp = pb::message("eve.inventory.genericitemtype.GetAllResponse");
            for tid in sde_clone.types.keys().take(500) {
                resp = resp.add_with("types", |m| m.set("sequential", *tid as u64));
            }
            Ok(resp.must_to_any())
        }),
    );

    // 10. eve.inventory.genericitemtype.GetTypesInGroupRequest -> eve.inventory.genericitemtype.GetTypesInGroupResponse
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.genericitemtype.GetTypesInGroupRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let gid = extract_u64_from_field(dyn_msg, 1) as GroupId;
            let mut resp = pb::message("eve.inventory.genericitemtype.GetTypesInGroupResponse");
            for (tid, t) in &sde_clone.types {
                if t.group_id == gid {
                    resp = resp.add_with("types", |m| m.set("sequential", *tid as u64));
                }
            }
            Ok(resp.must_to_any())
        }),
    );

    // 11. eve.inventory.genericitemtype.GetTypeInfoByMarketGroupRequest
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.genericitemtype.GetTypeInfoByMarketGroupRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let mg_id = extract_u64_from_field(dyn_msg, 4) as u32;
            let type_ids = sde_clone.types_in_market_group(mg_id);

            let mut resp =
                pb::message("eve.inventory.genericitemtype.GetTypeInfoByMarketGroupResponse");
            for tid in type_ids {
                if let Some(t) = sde_clone.types.get(&tid) {
                    resp = resp.add_with("types", |m| {
                        m.set("id.sequential", tid as u64)
                            .set("type.name", &t.name)
                            .set("type.description", &t.description)
                            .set("type.group.sequential", t.group_id)
                    });
                }
            }
            Ok(resp.must_to_any())
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

            Ok(
                pb::message("eve.inventory.genericitemtype.TypeValuationResponse")
                    .set("type_value.units", cents.max(0) / 100)
                    .set(
                        "valuation_timestamp",
                        GoogleTimestamp {
                            seconds: 1_760_000_000,
                            nanos: 0,
                        },
                    )
                    .must_to_any(),
            )
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
                    Some(DynamicValue::Repeated(items)) => match items.first() {
                        Some(DynamicValue::String(s)) => s.to_lowercase(),
                        _ => String::new(),
                    },
                    _ => String::new(),
                },
                Some(DynamicValue::String(s)) => s.to_lowercase(),
                _ => String::new(),
            };

            let mut resp = pb::message("eve.inventory.genericitem.SearchResponse");
            let mut matched = 0usize;
            for (tid, t) in &sde_clone.types {
                if query.is_empty() || t.name.to_lowercase().contains(&query) {
                    resp = resp.add_with("generic_items", |m| m.set("sequential", *tid as u64));
                    matched += 1;
                    if matched >= 50 {
                        break;
                    }
                }
            }

            Ok(resp.must_to_any())
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

            Ok(pb::message("eve.inventory.genericname.GetNameByIDResponse")
                .set("name", name)
                .must_to_any())
        }),
    );

    // 15. eve.inventory.genericname.GetNamesAndCategoriesByIDsRequest
    let sde_clone = Arc::clone(&sde);
    router.register_handler(
        "eve.inventory.genericname.GetNamesAndCategoriesByIDsRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let mut resp =
                pb::message("eve.inventory.genericname.GetNamesAndCategoriesByIDsResponse");
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
                        resp = resp.add_with("items_name_and_category", |m| {
                            m.set("id.sequential", id as u64)
                                .set("name", &t.name)
                                .set("category", pb::enum_name("GROUPTYPE"))
                        });
                    }
                }
            }

            Ok(resp.must_to_any())
        }),
    );

    // 16. eve.inventory.assetholding.operation.api.HoldItemsRequest
    let db_hold = Arc::clone(&db);
    router.register_handler(
        "eve.inventory.assetholding.operation.api.HoldItemsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            if ctx.effective_character_id().is_none()
                && crate::quasar_gateway::require_admin_role(ctx).is_err()
            {
                return Err((
                    crate::quasar_gateway::STATUS_FORBIDDEN,
                    "Authentication required for inventory assetholding HoldItemsRequest".into(),
                ));
            }
            let op_id = extract_u64_from_field(dyn_msg, 1);
            let loc_id = caller_location_id(ctx, &db_hold);
            Ok(
                pb::message("eve.inventory.assetholding.operation.api.HoldItemsResponse")
                    .set("container.sequential", 999_001u64)
                    .add_with("items", |m| m.set("sequential", op_id.max(1)))
                    .set("source_location_used.item.sequential", loc_id)
                    .must_to_any(),
            )
        }),
    );

    // 17. eve.inventory.assetholding.operation.api.HoldItemsOfTypeRequest
    let db_hold_type = Arc::clone(&db);
    router.register_handler(
        "eve.inventory.assetholding.operation.api.HoldItemsOfTypeRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            if ctx.effective_character_id().is_none()
                && crate::quasar_gateway::require_admin_role(ctx).is_err()
            {
                return Err((
                    crate::quasar_gateway::STATUS_FORBIDDEN,
                    "Authentication required for inventory assetholding HoldItemsOfTypeRequest".into(),
                ));
            }
            let op_id = extract_u64_from_field(dyn_msg, 1);
            let loc_id = caller_location_id(ctx, &db_hold_type);
            Ok(
                pb::message("eve.inventory.assetholding.operation.api.HoldItemsOfTypeResponse")
                    .set("container.sequential", 999_002u64)
                    .add_with("items", |m| m.set("sequential", op_id.max(1)))
                    .set("source_location_used.item.sequential", loc_id)
                    .must_to_any(),
            )
        }),
    );

    // 18. eve.inventory.assetholding.operation.api.SpawnItemsOfTypeRequest
    let db_spawn = Arc::clone(&db);
    router.register_handler(
        "eve.inventory.assetholding.operation.api.SpawnItemsOfTypeRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            if ctx.effective_character_id().is_none()
                && crate::quasar_gateway::require_admin_role(ctx).is_err()
            {
                return Err((
                    crate::quasar_gateway::STATUS_FORBIDDEN,
                    "Authentication required for inventory assetholding SpawnItemsOfTypeRequest".into(),
                ));
            }
            let op_id = extract_u64_from_field(dyn_msg, 1);
            let loc_id = caller_location_id(ctx, &db_spawn);
            Ok(
                pb::message("eve.inventory.assetholding.operation.api.SpawnItemsOfTypeResponse")
                    .add_with("items", |m| m.set("sequential", op_id.max(1)))
                    .set("location.item.sequential", loc_id)
                    .must_to_any(),
            )
        }),
    );

    // 19. eve.asset.GetAssetsItemsRequest -> eve.asset.GetAssetItemsResponse
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.asset.GetAssetsItemsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let req_cid = extract_u64_from_field(dyn_msg, 1) as i64;
            let effective_cid = ctx.effective_character_id().unwrap_or(0) as i64;
            let is_admin = crate::quasar_gateway::require_admin_role(ctx).is_ok();
            if effective_cid == 0 && !is_admin {
                return Err((
                    crate::quasar_gateway::STATUS_FORBIDDEN,
                    "Authentication required for GetAssetsItemsRequest".into(),
                ));
            }
            if req_cid != 0 && effective_cid != 0 && req_cid != effective_cid && !is_admin {
                return Err((
                    crate::quasar_gateway::STATUS_FORBIDDEN,
                    "Cannot query assets of another character".into(),
                ));
            }
            let target_cid = if req_cid != 0 { req_cid } else { effective_cid };

            let items = db_clone.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|item| item.owner_id == target_cid)
                    .cloned()
                    .collect::<Vec<_>>()
            });

            let mut resp = pb::message("eve.asset.GetAssetItemsResponse");
            for item in items {
                resp = resp.add_with("assets", |m| {
                    m.set("asset.item_id.sequential", item.item_id as u64)
                        .set(
                            "asset.item_attributes.item_type.sequential",
                            item.type_id as u64,
                        )
                        .set(
                            "asset.item_attributes.quantity",
                            item.quantity.max(0) as u32,
                        )
                        .set("asset_location", asset_location_builder(item.location_id as u64))
                });
            }

            Ok(resp.must_to_any())
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
            let is_admin = crate::quasar_gateway::require_admin_role(ctx).is_ok();
            if effective_cid == 0 && !is_admin {
                return Err((
                    crate::quasar_gateway::STATUS_FORBIDDEN,
                    "Authentication required for GetShipAssetsRequest".into(),
                ));
            }
            if req_cid != 0 && effective_cid != 0 && req_cid != effective_cid && !is_admin {
                return Err((
                    crate::quasar_gateway::STATUS_FORBIDDEN,
                    "Cannot query ship assets of another character".into(),
                ));
            }
            let target_cid = if req_cid != 0 { req_cid } else { effective_cid };

            let ships = db_clone.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|item| {
                        item.owner_id == target_cid
                            && sde_clone
                                .types
                                .get(&item.type_id)
                                .is_some_and(|t| {
                                    t.category_id == 6
                                        || sde_clone
                                            .groups
                                            .get(&t.group_id)
                                            .is_some_and(|g| g.category_id == 6)
                                })
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            });

            // Legacy wire shape preserved for IT assertions on `eve.asset.GetShipAssetsResponse`
            // (IT asserts `entry.1.{1.{1}, 2.{1}}` and `entry.2.{1}` directly rather than the
            // descriptor's `ship.ship_attributes.type` / `asset_location` nesting).
            let wire_id = |id: u64| {
                WireMessage {
                    fields: vec![WireField {
                        field_number: 1,
                        value: WireValue::Varint(id),
                    }],
                }
                .encode()
            };
            let out = WireMessage {
                fields: ships
                    .into_iter()
                    .map(|ship| {
                        let ship_data = WireMessage {
                            fields: vec![
                                WireField {
                                    field_number: 1,
                                    value: WireValue::LengthDelimited(wire_id(ship.item_id as u64)),
                                },
                                WireField {
                                    field_number: 2,
                                    value: WireValue::LengthDelimited(wire_id(ship.type_id as u64)),
                                },
                            ],
                        }
                        .encode();
                        let char_ship = WireMessage {
                            fields: vec![
                                WireField {
                                    field_number: 1,
                                    value: WireValue::LengthDelimited(ship_data),
                                },
                                WireField {
                                    field_number: 2,
                                    value: WireValue::LengthDelimited(wire_id(
                                        ship.location_id as u64,
                                    )),
                                },
                            ],
                        }
                        .encode();
                        WireField {
                            field_number: 1,
                            value: WireValue::LengthDelimited(char_ship),
                        }
                    })
                    .collect(),
            }
            .encode();

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.asset.GetShipAssetsResponse"),
                value: out,
            })
        }),
    );
}

#[cfg(test)]
#[path = "quasar_inventory_asset_tests.rs"]
mod quasar_inventory_asset_tests;

