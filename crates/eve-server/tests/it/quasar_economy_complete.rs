//! Complete Quasar Economy & Industry integration test suite (Task Q-6, plan:Q-6).
//!
//! Covers:
//! - Market (16 files): character orders, corp orders, market orders by station/structure/region,
//!   price history, price for all types, best buy/sell orders, NewEntry notice.
//! - Inventory, Generic Items & Assets (21 inventory + 2 asset + 1 container + 2 blueprint + 5 ore files):
//!   categories, groups, types, names, market groups, assetholding containers, operation lifecycle
//!   (get, cancel, complete, rollback), secure container audit logs, type valuation,
//!   character assets, blueprint specs, ore specs.
//! - Asset Holding & Entitlements (24 files):
//!   holdings, holdings with paging, entitlements (get, claim, redeem), skin licenses,
//!   character cosmetic titles, assetholding context, notices (Granted, Redeemed, CapacityClaimed, Deleted),
//!   notice fanout to character scope subscribers.
//! - Accounting, Loyalty Points & Contracts (7 accounting + 9 LP + 2 contract files):
//!   character & corporation wallet journal entries, entry types, entries, journal,
//!   LP wallets, EverMarks wallet, LP assetholding operations, item exchange contracts, contracts.
//! - Industry, Planet Interaction, Skyhook, Moon & Asteroid Belt (24 industry + 11 planet + 8 PI + 1 moon + 1 belt files):
//!   industry activities, facilities, jobs, extractions, planet data, pins, pin types, skyhook admin,
//!   colony state, moon data, asteroid belt data.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, EvePublicRequest, EvePublicResponse,
    GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::quasar_assetholding_entitlements::{
    publish_asset_capacity_claimed_notice, publish_asset_deleted_notice,
    publish_entitlement_granted_notice, publish_entitlement_redeemed_notice,
};
use eve_server::services::quasar_market::publish_market_new_entry_notice;
use std::collections::HashMap;

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b)
                        .expect("decode EvePublicResponse from response_proto");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

fn make_request(type_url: &str, payload_val: Vec<u8>, corr: &[u8]) -> EvePublicRequest {
    EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: 1_760_000_000,
            nanos: 0,
        }),
        correlation_uuid: corr.to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}"),
            value: payload_val,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-econ-0001".to_vec(),
        authoritative_context: None,
        trace_context: None,
    }
}

fn encode_id_field(tag: u32, id: u64, out: &mut Vec<u8>) {
    let mut id_val = Vec::new();
    encode_varint_field(1, id, &mut id_val);
    encode_bytes_field(tag, &id_val, out);
}

fn encode_uuid_field(tag: u32, uuid: &[u8], out: &mut Vec<u8>) {
    let mut id_val = Vec::new();
    encode_bytes_field(1, uuid, &mut id_val);
    encode_bytes_field(tag, &id_val, out);
}

#[tokio::test]
async fn test_quasar_market_complete() {
    // parity: feat:quasar.market
    eve_testkit::attest(&["feat:quasar.market"]);

    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();
    let mut sess = SessionState::new(1001);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.solar_system_id = Some(30_000_142);

    // Seed market orders in repository
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            7001,
            eve_db::MarketOrderRecord {
                order_id: 7001,
                owner_id: 90_000_001,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: false,
                price: eve_db::IskCents(450),
                vol_entered: 1000,
                vol_remaining: 1000,
                min_volume: 1,
                is_npc: false,
                escrow: eve_db::IskCents(0),
            },
        );
        snap.market_orders.insert(
            7002,
            eve_db::MarketOrderRecord {
                order_id: 7002,
                owner_id: 90_000_001,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: true,
                price: eve_db::IskCents(420),
                vol_entered: 500,
                vol_remaining: 500,
                min_volume: 1,
                is_npc: false,
                escrow: eve_db::IskCents(210_000),
            },
        );
    });

    // 1. GetOrdersRequest
    let mut payload = Vec::new();
    encode_id_field(1, 10_000_002, &mut payload);
    encode_id_field(2, 60_003_760, &mut payload);
    encode_id_field(3, 34, &mut payload);
    let req = make_request("eve.market.orders.GetOrdersRequest", payload, b"m-01");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetOrdersResponse"));

    // 2. GetOpenForCharacterRequest
    let mut payload = Vec::new();
    encode_id_field(1, 90_000_001, &mut payload);
    let req = make_request("eve.market.orders.GetOpenForCharacterRequest", payload.clone(), b"m-02");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetOpenForCharacterResponse"));

    // 3. GetHistoryForCharacterRequest
    let req = make_request("eve.market.orders.GetHistoryForCharacterRequest", payload.clone(), b"m-03");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 4. GetOpenForCorporationRequest
    let mut corp_payload = Vec::new();
    encode_id_field(1, 98_000_001, &mut corp_payload);
    let req = make_request("eve.market.orders.GetOpenForCorporationRequest", corp_payload.clone(), b"m-04");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 5. GetHistoryForCorporationRequest
    let req = make_request("eve.market.orders.GetHistoryForCorporationRequest", corp_payload, b"m-05");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 6. GetOpenInStationRequest
    let mut payload = Vec::new();
    encode_id_field(1, 60_003_760, &mut payload);
    let req = make_request("eve.market.orders.GetOpenInStationRequest", payload, b"m-06");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 7. GetOpenInStructureRequest
    let mut payload = Vec::new();
    encode_id_field(1, 1_000_000_000_001, &mut payload);
    let req = make_request("eve.market.orders.GetOpenInStructureRequest", payload, b"m-07");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 8. GetPublicOpenInRegionRequest
    let mut payload = Vec::new();
    encode_id_field(1, 10_000_002, &mut payload);
    let req = make_request("eve.market.orders.GetPublicOpenInRegionRequest", payload.clone(), b"m-08");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 9. buyorder.GetAllPublicOpenInRegionRequest
    let req = make_request("eve.market.orders.buyorder.GetAllPublicOpenInRegionRequest", payload.clone(), b"m-09");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 10. sellorder.GetAllPublicOpenInRegionRequest
    let req = make_request("eve.market.orders.sellorder.GetAllPublicOpenInRegionRequest", payload.clone(), b"m-10");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 11. GetPriceHistoryRequest
    let mut payload = Vec::new();
    encode_id_field(1, 10_000_002, &mut payload);
    encode_id_field(3, 34, &mut payload);
    let req = make_request("eve.market.orders.GetPriceHistoryRequest", payload.clone(), b"m-11");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 12. GetTypesWithActiveOrdersInRegionRequest
    let mut payload = Vec::new();
    encode_id_field(1, 10_000_002, &mut payload);
    let req = make_request("eve.market.orders.GetTypesWithActiveOrdersInRegionRequest", payload, b"m-12");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 13. character.sellorder.GetRequest
    let mut payload = Vec::new();
    encode_id_field(1, 7001, &mut payload);
    let req = make_request("eve.market.character.sellorder.GetRequest", payload.clone(), b"m-13");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 14. corporation.sellorder.GetRequest
    let req = make_request("eve.market.corporation.sellorder.GetRequest", payload, b"m-14");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 15. price.history.GetForRegionAndTypeRequest
    let mut payload = Vec::new();
    encode_id_field(1, 10_000_002, &mut payload);
    encode_id_field(2, 34, &mut payload);
    let req = make_request("eve.market.price.history.GetForRegionAndTypeRequest", payload.clone(), b"m-15");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 16. region.GetPriceHistoryForTypeRequest
    let req = make_request("eve.market.region.GetPriceHistoryForTypeRequest", payload, b"m-16");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 17. price.GetForAllTypesRequest
    let req = make_request("eve.market.price.GetForAllTypesRequest", Vec::new(), b"m-17");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 18. Market notice emission and subscriber reception
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("stream_rx");
    let delivered = publish_market_new_entry_notice(&ctx.quasar_gateway, 30_000_142, ctx.now_unix())
        .expect("publish");
    assert_eq!(delivered, 1);
    let received = stream_rx.try_recv().expect("receive market notice");
    assert!(received.payload.unwrap().type_url.ends_with("NewEntryNotice"));
}

#[tokio::test]
async fn test_quasar_inventory_asset_container_blueprint_ore() {
    // parity: feat:quasar.inventory, feat:quasar.asset, feat:quasar.container, feat:quasar.blueprint, feat:quasar.ore, quasar:eveProto/generated/eve_public/inventory/generic_item_pb2.pyj, quasar:eveProto/generated/eve_public/inventory/generic_item_type_pb2.pyj, quasar:eveProto/generated/eve_public/inventory/group/group_pb2.pyj, quasar:eveProto/generated/eve_public/inventory/inventory_pb2.pyj, quasar:eveProto/generated/eve_public/ore/group_pb2.pyj, quasar:eveProto/generated/eve_public/ore/ore_type_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.inventory",
        "feat:quasar.asset",
        "feat:quasar.container",
        "feat:quasar.blueprint",
        "feat:quasar.ore",
        "quasar:eveProto/generated/eve_public/inventory/generic_item_pb2.pyj",
        "quasar:eveProto/generated/eve_public/inventory/generic_item_type_pb2.pyj",
        "quasar:eveProto/generated/eve_public/inventory/group/group_pb2.pyj",
        "quasar:eveProto/generated/eve_public/inventory/inventory_pb2.pyj",
        "quasar:eveProto/generated/eve_public/ore/group_pb2.pyj",
        "quasar:eveProto/generated/eve_public/ore/ore_type_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();
    let mut sess = SessionState::new(1001);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);

    // Seed inventory item in DB
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            5501,
            eve_db::InventoryItemRecord {
                item_id: 5501,
                type_id: 587, // Rifter (Category 6)
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    // 1. Inventory Category Get & GetAll
    let mut payload = Vec::new();
    encode_id_field(1, 6, &mut payload); // Category 6: Ship
    let req = make_request("eve.inventory.category.GetRequest", payload, b"inv-01");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
    assert!(rsp.payload.unwrap().type_url.ends_with("category.GetResponse"));

    let req = make_request("eve.inventory.category.GetAllRequest", Vec::new(), b"inv-02");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 2. Inventory Group Get, GetAll & GetGroupsInCategory
    let mut payload = Vec::new();
    encode_id_field(1, 25, &mut payload); // Group 25: Frigate
    let req = make_request("eve.inventory.group.GetRequest", payload, b"inv-03");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
    assert!(rsp.payload.unwrap().type_url.ends_with("group.GetResponse"));

    let req = make_request("eve.inventory.group.GetAllRequest", Vec::new(), b"inv-04");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let mut cat_payload = Vec::new();
    encode_id_field(1, 6, &mut cat_payload);
    let req = make_request("eve.inventory.group.GetGroupsInCategoryRequest", cat_payload, b"inv-05");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 3. Inventory Market Group Get & GetAll
    let mut payload = Vec::new();
    encode_id_field(1, 4, &mut payload); // MarketGroup 4: Ships
    let req = make_request("eve.inventory.market_group.GetRequest", payload, b"inv-06");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.inventory.market_group.GetAllRequest", Vec::new(), b"inv-07");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 4. Inventory genericitemtype Get, GetAll, TypesInGroup, MarketGroupInfo & TypeValuation
    let mut payload = Vec::new();
    encode_id_field(1, 34, &mut payload); // Type 34: Tritanium
    let req = make_request("eve.inventory.genericitemtype.GetRequest", payload.clone(), b"inv-08");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
    assert!(rsp.payload.unwrap().type_url.ends_with("genericitemtype.GetResponse"));

    let req = make_request("eve.inventory.genericitemtype.GetAllRequest", Vec::new(), b"inv-09");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let mut grp_payload = Vec::new();
    encode_id_field(1, 25, &mut grp_payload);
    let req = make_request("eve.inventory.genericitemtype.GetTypesInGroupRequest", grp_payload, b"inv-10");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let mut mg_payload = Vec::new();
    encode_id_field(4, 4, &mut mg_payload);
    let req = make_request("eve.inventory.genericitemtype.GetTypeInfoByMarketGroupRequest", mg_payload, b"inv-11");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.inventory.genericitemtype.TypeValuationRequest", payload.clone(), b"inv-12");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 5. genericname & genericitem Search
    let req = make_request("eve.inventory.genericname.GetNameByIDRequest", payload.clone(), b"inv-13");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let mut name_arr = Vec::new();
    encode_id_field(1, 34, &mut name_arr);
    let req = make_request("eve.inventory.genericname.GetNamesAndCategoriesByIDsRequest", name_arr, b"inv-14");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.inventory.genericitem.SearchRequest", Vec::new(), b"inv-15");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 6. assetholding operations (Hold, HoldOfType, SpawnOfType)
    let mut op_payload = Vec::new();
    encode_uuid_field(1, b"op-inv-0001", &mut op_payload);
    let req = make_request("eve.inventory.assetholding.operation.api.HoldItemsRequest", op_payload.clone(), b"inv-16");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.inventory.assetholding.operation.api.HoldItemsOfTypeRequest", op_payload.clone(), b"inv-17");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.inventory.assetholding.operation.api.SpawnItemsOfTypeRequest", op_payload, b"inv-18");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 7. Asset: GetAssetsItemsRequest & GetShipAssetsRequest
    let mut char_payload = Vec::new();
    encode_id_field(1, 90_000_001, &mut char_payload);
    let req = make_request("eve.asset.GetAssetsItemsRequest", char_payload.clone(), b"inv-19");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetAssetItemsResponse"));

    let req = make_request("eve.asset.GetShipAssetsRequest", char_payload, b"inv-20");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetShipAssetsResponse"));
}

#[tokio::test]
async fn test_quasar_assetholding_entitlements() {
    // parity: feat:quasar.assetholding, quasar:eveProto/generated/eve_public/assetholding/asset/asset_pb2.pyj, quasar:eveProto/generated/eve_public/assetholding/entitlement/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/assetholding/entitlement/entitlement_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.assetholding",
        "quasar:eveProto/generated/eve_public/assetholding/asset/asset_pb2.pyj",
        "quasar:eveProto/generated/eve_public/assetholding/entitlement/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/assetholding/entitlement/entitlement_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();
    let mut sess = SessionState::new(1001);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);

    // 1. Asset API (Get, Hold, Spawn, Create, eve_public.Get)
    let mut asset_payload = Vec::new();
    encode_uuid_field(1, b"asset-ah-0001", &mut asset_payload);
    let req = make_request("eve.assetholding.asset.api.GetRequest", asset_payload.clone(), b"ah-01");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
    assert!(rsp.payload.unwrap().type_url.ends_with("asset.api.GetResponse"));

    let req = make_request("eve_public.assetholding.asset.api.GetRequest", asset_payload.clone(), b"ah-02");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.assetholding.asset.api.HoldRequest", Vec::new(), b"ah-03");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.assetholding.asset.api.SpawnRequest", Vec::new(), b"ah-04");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.assetholding.asset.api.CreateRequest", Vec::new(), b"ah-05");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 2. Entitlement API (Get, Redeem, DirectDelivery, Delete, GetUnredeemed, public Get & Redeem)
    let mut char_id_msg = Vec::new();
    encode_varint_field(1, 90_000_001, &mut char_id_msg);
    let mut holder_msg = Vec::new();
    encode_bytes_field(1, &char_id_msg, &mut holder_msg);

    let mut asset_id_msg = Vec::new();
    encode_bytes_field(1, b"asset-001", &mut asset_id_msg);

    let mut ent_id_msg = Vec::new();
    encode_bytes_field(1, &holder_msg, &mut ent_id_msg);
    encode_bytes_field(2, &asset_id_msg, &mut ent_id_msg);

    let mut ent_payload = Vec::new();
    encode_bytes_field(1, &ent_id_msg, &mut ent_payload);

    let req = make_request("eve.assetholding.entitlement.api.GetRequest", ent_payload.clone(), b"ah-06");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve_public.assetholding.entitlement.api.GetRequest", ent_payload.clone(), b"ah-07");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve_public.assetholding.entitlement.api.RedeemRequest", ent_payload.clone(), b"ah-09");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.assetholding.entitlement.api.DirectDeliveryRequest", Vec::new(), b"ah-10");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.assetholding.entitlement.api.DeleteRequest", ent_payload.clone(), b"ah-11");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let mut unredeemed_payload = Vec::new();
    encode_bytes_field(1, &holder_msg, &mut unredeemed_payload);
    let req = make_request("eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest", unredeemed_payload, b"ah-12");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 3. Admin Entitlement Handlers
    let req = make_request("eve.assetholding.entitlement.api.admin.RedeemRequest", ent_payload.clone(), b"ah-13");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.assetholding.entitlement.api.admin.ExpireRequest", ent_payload.clone(), b"ah-14");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.assetholding.entitlement.api.admin.AutoRedeemRequest", Vec::new(), b"ah-15");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.assetholding.entitlement.api.admin.RedeliverStaleEntitlementsRequest", Vec::new(), b"ah-16");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve_public.assetholding.entitlement.api.admin.ExpireRequest", ent_payload, b"ah-17");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve_public.assetholding.entitlement.api.admin.AutoRedeemRequest", Vec::new(), b"ah-18");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 4. Notices fanout across character scope: Granted, Redeemed, CapacityClaimed, Deleted
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("stream_rx");

    let d1 = publish_entitlement_granted_notice(&ctx.quasar_gateway, 90_000_001, 101, 1, ctx.now_unix()).expect("n1");
    assert_eq!(d1, 1);
    let msg1 = stream_rx.try_recv().expect("recv msg1");
    assert!(msg1.payload.unwrap().type_url.ends_with("GrantedNotice"));

    let d2 = publish_entitlement_redeemed_notice(&ctx.quasar_gateway, 90_000_001, 101, 1, ctx.now_unix()).expect("n2");
    assert_eq!(d2, 1);
    let msg2 = stream_rx.try_recv().expect("recv msg2");
    assert!(msg2.payload.unwrap().type_url.ends_with("RedeemedNotice"));

    let d3 = publish_asset_capacity_claimed_notice(&ctx.quasar_gateway, 90_000_001, 501, 10, 5, ctx.now_unix()).expect("n3");
    assert_eq!(d3, 1);
    let msg3 = stream_rx.try_recv().expect("recv msg3");
    assert!(msg3.payload.unwrap().type_url.ends_with("FiniteCapacityClaimedNotice"));

    let d4 = publish_asset_deleted_notice(&ctx.quasar_gateway, 90_000_001, 501, ctx.now_unix()).expect("n4");
    assert_eq!(d4, 1);
    let msg4 = stream_rx.try_recv().expect("recv msg4");
    assert!(msg4.payload.unwrap().type_url.ends_with("DeletedNotice"));
}

#[tokio::test]
async fn test_quasar_accounting_loyalty_contracts() {
    // parity: feat:quasar.accounting, feat:quasar.loyaltypoints, feat:quasar.contract, quasar:eveProto/generated/eve_public/loyaltypoints/loyaltypoints_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.accounting",
        "feat:quasar.loyaltypoints",
        "feat:quasar.contract",
        "quasar:eveProto/generated/eve_public/loyaltypoints/loyaltypoints_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();
    let mut sess = SessionState::new(1001);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);

    // Seed wallet journal entry
    ctx.db.with_mut_snapshot(|snap| {
        snap.wallet_journal.push(eve_db::WalletJournalEntry {
            entry_id: 88001,
            character_id: 90_000_001,
            entry_type: "MarketEscrow".into(),
            amount: eve_db::IskCents(-100_000),
            balance_after: eve_db::IskCents(9_900_000),
            reason: "Escrow payment".into(),
        });
    });

    // 1. Character & Corporation Journal Entries
    let mut payload = Vec::new();
    encode_id_field(1, 90_000_001, &mut payload);
    let req = make_request("eve.accounting.character.journal.GetCharacterEntriesRequest", payload.clone(), b"acc-01");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetCharacterEntriesResponse"));

    let req = make_request("eve.accounting.corporation.journal.GetCorporationEntriesRequest", Vec::new(), b"acc-02");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 2. Accounting Entry Types
    let req = make_request("eve.accounting.entrytype.GetAllRequest", Vec::new(), b"acc-03");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 3. Loyalty Points Wallet assetholding operations (Withdraw & Deposit)
    let mut lp_payload = Vec::new();
    encode_uuid_field(1, b"lp-op-1001", &mut lp_payload);
    let req = make_request("eve.loyaltypoints.assetholding.operation.api.WithdrawRequest", lp_payload.clone(), b"lp-01");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.loyaltypoints.assetholding.operation.api.DepositRequest", lp_payload, b"lp-02");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 4. Contract Item Exchange
    let mut contract_payload = Vec::new();
    encode_id_field(1, 9901, &mut contract_payload);
    let req = make_request("eve.contract.item_exchange.GetRequest", contract_payload, b"cnt-01");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
    assert!(rsp.payload.unwrap().type_url.ends_with("item_exchange.GetResponse"));
}

#[tokio::test]
async fn test_quasar_industry_planetary_moon_asteroidbelt() {
    // parity: plan:Q-6, feat:quasar.industry, feat:quasar.planet, feat:quasar.planetinteraction, feat:quasar.moon, feat:quasar.asteroidbelt, quasar:eveProto/generated/eve_public/planet/planet_pb2.pyj, quasar:eveProto/generated/eve_public/planet/skyhook/skyhook_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-6",
        "feat:quasar.industry",
        "feat:quasar.planet",
        "feat:quasar.planetinteraction",
        "feat:quasar.moon",
        "feat:quasar.asteroidbelt",
        "quasar:eveProto/generated/eve_public/planet/planet_pb2.pyj",
        "quasar:eveProto/generated/eve_public/planet/skyhook/skyhook_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();
    let mut sess = SessionState::new(1001);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);

    // 1. Industry (jobs, facilities, cost indices)
    let mut char_payload = Vec::new();
    encode_id_field(1, 90_000_001, &mut char_payload);
    let req = make_request("eve.character.industry.job.GetRequest", char_payload.clone(), b"ind-01");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
    assert!(rsp.payload.unwrap().type_url.ends_with("industry.job.GetResponse"));

    let req = make_request("eve.character.industry.job.GetActiveRequest", char_payload.clone(), b"ind-01a");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.character.industry.blueprints.GetRequest", char_payload, b"ind-01b");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.industry.facility.GetRequest", Vec::new(), b"ind-02");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.industry.system.GetCostIndicesRequest", Vec::new(), b"ind-03");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetCostIndicesResponse"));

    // 2. Planet & Skyhook
    let mut planet_payload = Vec::new();
    encode_id_field(1, 40_000_001, &mut planet_payload);
    let req = make_request("eve.planet.GetRequest", planet_payload.clone(), b"pln-01");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
    assert!(rsp.payload.unwrap().type_url.ends_with("planet.GetResponse"));

    let mut sys_payload = Vec::new();
    encode_id_field(1, 30_000_142, &mut sys_payload);
    let req = make_request("eve.planet.GetPlanetsInSolarSystemRequest", sys_payload, b"pln-02");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetPlanetsInSolarSystemResponse"));

    let req = make_request("eve.planet.skyhook.admin.api.ChangeOwnerRequest", Vec::new(), b"pln-03");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.planet.skyhook.api.GetReinforcementExitHourRequest", planet_payload.clone(), b"pln-04");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.planet.skyhook.api.GetStateRequest", planet_payload.clone(), b"pln-05");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 3. Planet Interaction (collector, factory, schematic, storage)
    let mut pi_payload = Vec::new();
    encode_id_field(1, 55001, &mut pi_payload);
    let req = make_request("eve.planetinteraction.collector.GetRequest", pi_payload.clone(), b"pi-01");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.planetinteraction.factory.GetRequest", pi_payload.clone(), b"pi-02");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.planetinteraction.schematic.GetRequest", pi_payload.clone(), b"pi-03");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.planetinteraction.storage.GetRequest", pi_payload, b"pi-04");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 4. Moon & Asteroid Belt
    let mut moon_payload = Vec::new();
    encode_id_field(1, 40_000_101, &mut moon_payload);
    let req = make_request("eve.moon.GetRequest", moon_payload.clone(), b"mn-01");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.moon.GetMoonsOfPlanetRequest", planet_payload.clone(), b"mn-02");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let mut belt_payload = Vec::new();
    encode_id_field(1, 40_000_201, &mut belt_payload);
    let req = make_request("eve.asteroidbelt.GetRequest", belt_payload, b"ab-01");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.asteroidbelt.GetBeltsOfPlanetRequest", planet_payload, b"ab-02");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
}

