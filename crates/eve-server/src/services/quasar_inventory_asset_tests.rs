//! Unit and mutation-killing tests for `quasar_inventory_asset` (`Q-6`).

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord, WorldRepository};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::{MarketGroupInfo, SdeRegistry};
use std::sync::{Arc, OnceLock};

const ROLE_PROGRAMMER: u64 = eve_machonet::service_roles::ROLE_GM_MASK;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut s = SdeRegistry::with_canonical_seed();
        s.market_groups.entry(1857).or_insert(MarketGroupInfo {
            market_group_id: 1857,
            parent_group_id: None,
            name: "Minerals".to_string(),
            has_types: true,
            icon_id: None,
        });
        s.market_groups.entry(1858).or_insert(MarketGroupInfo {
            market_group_id: 1858,
            parent_group_id: Some(1857),
            name: "Basic Minerals".to_string(),
            has_types: true,
            icon_id: None,
        });
        Arc::new(s)
    }))
}

trait TestRouterExt {
    fn dispatch(
        &self,
        ctx: &AuthoritativeContext,
        req: &GoogleAny,
    ) -> Result<GoogleAny, (u32, String)>;
}
impl TestRouterExt for PublicGatewayRouter {
    fn dispatch(
        &self,
        ctx: &AuthoritativeContext,
        req: &GoogleAny,
    ) -> Result<GoogleAny, (u32, String)> {
        let norm = req.normalized_type_name();
        let handler = self
            .handlers
            .read()
            .unwrap()
            .get(norm)
            .cloned()
            .expect("registered handler");
        let dyn_msg = self
            .descriptors
            .decode_dynamic_or_wire(norm, &req.value)
            .unwrap_or_else(|_| {
                crate::services::quasar_char_skill_services::decode_wire_dynamic(norm, &req.value)
            });
        handler(ctx, req, &dyn_msg)
    }
}

fn make_router_and_db() -> (Arc<PublicGatewayRouter>, Arc<WorldRepository>, Arc<SdeRegistry>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let router = Arc::new(PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    ));
    (router, db, sde)
}

fn make_ctx(char_id: Option<u32>, role: u64) -> AuthoritativeContext {
    AuthoritativeContext::for_session_at(
        Some(1),
        char_id,
        None,
        0,
        "tranquility".to_string(),
        b"evomu-quasar-gw1".to_vec(),
        1_760_000_000,
    )
    .with_service_roles(role)
}

fn encode_id_req(type_name: &str, id: u64) -> GoogleAny {
    let mut id_msg = Vec::new();
    encode_varint_field(1, id, &mut id_msg);
    let mut req = Vec::new();
    encode_bytes_field(1, &id_msg, &mut req);
    GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value: req,
    }
}

#[test]
fn category_group_market_group_and_item_type_queries() {
    // parity: feat:inventory.containers
    let mut dm = DynamicQuasarMessage::new("test.Msg");
    dm.set(1, DynamicValue::Uint64(10));
    assert_eq!(extract_u64_from_field(&dm, 1), 10);
    dm.set(2, DynamicValue::Uint32(11));
    assert_eq!(extract_u64_from_field(&dm, 2), 11);
    dm.set(3, DynamicValue::Int64(12));
    assert_eq!(extract_u64_from_field(&dm, 3), 12);
    dm.set(4, DynamicValue::Int32(13));
    assert_eq!(extract_u64_from_field(&dm, 4), 13);

    let (router, db, _sde) = make_router_and_db();
    let user = make_ctx(Some(211200300), 0);

    // 1. Category GetRequest & 2. GetAllRequest
    assert_eq!(
        router
            .dispatch(&user, &encode_id_req("eve.inventory.category.GetRequest", 999_999_999))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert!(!router
        .dispatch(&user, &encode_id_req("eve.inventory.category.GetRequest", 4))
        .unwrap()
        .value
        .is_empty());
    assert!(!router
        .dispatch(&user, &encode_id_req("eve.inventory.category.GetRequest", 6))
        .unwrap()
        .value
        .is_empty());
    assert!(!router
        .dispatch(
            &user,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.category.GetAllRequest"),
                value: vec![],
            },
        )
        .unwrap()
        .value
        .is_empty());

    // 3. Group GetRequest, 4. GetAllRequest, 5. GetGroupsInCategoryRequest
    assert_eq!(
        router
            .dispatch(&user, &encode_id_req("eve.inventory.group.GetRequest", 999_999_999))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert!(!router
        .dispatch(&user, &encode_id_req("eve.inventory.group.GetRequest", 18))
        .unwrap()
        .value
        .is_empty());
    assert!(!router
        .dispatch(&user, &encode_id_req("eve.inventory.group.GetRequest", 25))
        .unwrap()
        .value
        .is_empty());
    assert!(!router
        .dispatch(
            &user,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.group.GetAllRequest"),
                value: vec![],
            },
        )
        .unwrap()
        .value
        .is_empty());
    assert!(!router
        .dispatch(
            &user,
            &encode_id_req("eve.inventory.group.GetGroupsInCategoryRequest", 4),
        )
        .unwrap()
        .value
        .is_empty());

    // 6. MarketGroup GetAllRequest & 7. GetRequest (root and child)
    assert_eq!(
        router
            .dispatch(&user, &encode_id_req("eve.inventory.market_group.GetRequest", 999_999_999))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert!(!router
        .dispatch(&user, &encode_id_req("eve.inventory.market_group.GetRequest", 1857))
        .unwrap()
        .value
        .is_empty());
    assert!(!router
        .dispatch(&user, &encode_id_req("eve.inventory.market_group.GetRequest", 1858))
        .unwrap()
        .value
        .is_empty());
    assert!(!router
        .dispatch(
            &user,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.market_group.GetAllRequest"),
                value: vec![],
            },
        )
        .unwrap()
        .value
        .is_empty());

    // 8. GenericItemType GetRequest, 9. GetAllRequest, 10. GetTypesInGroupRequest, 11. GetTypeInfoByMarketGroupRequest
    assert_eq!(
        router
            .dispatch(&user, &encode_id_req("eve.inventory.genericitemtype.GetRequest", 999_999_999))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert!(!router
        .dispatch(&user, &encode_id_req("eve.inventory.genericitemtype.GetRequest", 34))
        .unwrap()
        .value
        .is_empty());
    assert!(!router
        .dispatch(
            &user,
            &GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.genericitemtype.GetAllRequest"
                ),
                value: vec![],
            },
        )
        .unwrap()
        .value
        .is_empty());
    assert!(!router
        .dispatch(
            &user,
            &encode_id_req("eve.inventory.genericitemtype.GetTypesInGroupRequest", 18),
        )
        .unwrap()
        .value
        .is_empty());

    let mut mg_req = Vec::new();
    let mut mg_id = Vec::new();
    encode_varint_field(1, 1857, &mut mg_id);
    encode_bytes_field(4, &mg_id, &mut mg_req);
    assert!(!router
        .dispatch(
            &user,
            &GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.genericitemtype.GetTypeInfoByMarketGroupRequest"
                ),
                value: mg_req,
            },
        )
        .unwrap()
        .value
        .is_empty());

    // 12. TypeValuationRequest (fallback and market-order backed)
    db.try_with_mut_snapshot(|snap| {
        snap.market_orders.clear();
        Ok::<(), String>(())
    })
    .unwrap();
    let val_default = router
        .dispatch(
            &user,
            &encode_id_req("eve.inventory.genericitemtype.TypeValuationRequest", 34),
        )
        .unwrap();
    assert!(!val_default.value.is_empty());

    db.try_with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            1,
            MarketOrderRecord {
                order_id: 1,
                type_id: 34,
                owner_id: 211200300,
                station_id: 60003760,
                region_id: 10000002,
                price: IskCents::from_isk(950),
                vol_entered: 1000,
                vol_remaining: 1000,
                min_volume: 1,
                is_bid: false,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        Ok::<(), String>(())
    })
    .unwrap();
    let val_seeded = router
        .dispatch(
            &user,
            &encode_id_req("eve.inventory.genericitemtype.TypeValuationRequest", 34),
        )
        .unwrap();
    assert_ne!(val_default.value, val_seeded.value);
}

#[test]
fn search_genericname_assetholding_and_character_asset_handlers() {
    // parity: feat:inventory.containers
    let (router, db, _sde) = make_router_and_db();
    let cid = 211200310i64;
    let other_cid = 211200311i64;
    db.try_with_mut_snapshot(|snap| {
        let mut rec = snap.characters[&90_000_001].clone();
        rec.character_id = cid;
        snap.characters.insert(cid, rec);
        snap.items.insert(
            5001,
            InventoryItemRecord {
                item_id: 5001,
                type_id: 34,
                owner_id: cid,
                location_id: 60003760,
                flag: InventoryFlag::Hangar,
                quantity: 1000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            5002,
            InventoryItemRecord {
                item_id: 5002,
                type_id: 587,
                owner_id: cid,
                location_id: 60003760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        Ok::<(), String>(())
    })
    .unwrap();

    let anon = make_ctx(None, 0);
    let user = make_ctx(Some(cid as u32), 0);
    let other = make_ctx(Some(other_cid as u32), 0);
    let admin = make_ctx(None, ROLE_PROGRAMMER);

    // 13. SearchRequest
    let mut q_bytes = Vec::new();
    encode_string_field(1, "trit", &mut q_bytes);
    let search_resp = router
        .dispatch(
            &user,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.genericitem.SearchRequest"),
                value: q_bytes,
            },
        )
        .unwrap();
    assert!(!search_resp.value.is_empty());

    // 14. GetNameByIDRequest (known & unknown) & 15. GetNamesAndCategoriesByIDsRequest
    assert!(!router
        .dispatch(&user, &encode_id_req("eve.inventory.genericname.GetNameByIDRequest", 34))
        .unwrap()
        .value
        .is_empty());
    assert!(!router
        .dispatch(&user, &encode_id_req("eve.inventory.genericname.GetNameByIDRequest", 999))
        .unwrap()
        .value
        .is_empty());

    let mut multi_ids = Vec::new();
    let mut id1 = Vec::new();
    encode_varint_field(1, 34, &mut id1);
    let mut id2 = Vec::new();
    encode_varint_field(1, 587, &mut id2);
    encode_bytes_field(1, &id1, &mut multi_ids);
    encode_bytes_field(1, &id2, &mut multi_ids);
    let names_cats = router
        .dispatch(
            &user,
            &GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.inventory.genericname.GetNamesAndCategoriesByIDsRequest"
                ),
                value: multi_ids,
            },
        )
        .unwrap();
    assert!(!names_cats.value.is_empty());

    // 16, 17, 18. Inventory assetholding operation handlers
    for rpc in [
        "eve.inventory.assetholding.operation.api.HoldItemsRequest",
        "eve.inventory.assetholding.operation.api.HoldItemsOfTypeRequest",
        "eve.inventory.assetholding.operation.api.SpawnItemsOfTypeRequest",
    ] {
        let req = encode_id_req(rpc, 42);
        assert_eq!(router.dispatch(&anon, &req).unwrap_err().0, STATUS_FORBIDDEN);
        assert!(!router.dispatch(&user, &req).unwrap().value.is_empty());
        assert!(!router.dispatch(&admin, &req).unwrap().value.is_empty());
    }

    // 19. GetAssetsItemsRequest & 20. GetShipAssetsRequest
    for rpc in ["eve.asset.GetAssetsItemsRequest", "eve.asset.GetShipAssetsRequest"] {
        let req = encode_id_req(rpc, cid as u64);
        assert_eq!(router.dispatch(&anon, &req).unwrap_err().0, STATUS_FORBIDDEN);
        assert_eq!(router.dispatch(&other, &req).unwrap_err().0, STATUS_FORBIDDEN);
        assert!(!router.dispatch(&user, &req).unwrap().value.is_empty());
        assert!(!router.dispatch(&admin, &req).unwrap().value.is_empty());
    }
}
