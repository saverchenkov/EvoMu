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

use crate::common;
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, DynamicQuasarMessage, DynamicValue, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, QuasarMessage,
    WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::quasar_assetholding_entitlements::{
    publish_asset_capacity_claimed_notice, publish_asset_deleted_notice,
};
use eve_server::services::quasar_market::publish_market_new_entry_notice;
use eve_testkit::client_harness::ClientErrorResponse;
use std::collections::HashMap;
use std::sync::Arc;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, ClientErrorResponse> {
    let kw = HashMap::new();
    common::borrow_wire_harness(ctx, sess)
        .call(service, None, method, args, &kw)
        .await
}

async fn login_pilot_char(
    ctx: &ServerContext,
    sess_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    sess
}

fn reg() -> Arc<QuasarDescriptorRegistry> {
    QuasarDescriptorRegistry::open_default().expect("open default QuasarDescriptorRegistry")
}

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

async fn quasar_send(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_url: &str,
    payload: Vec<u8>,
    corr: &[u8],
) -> EvePublicResponse {
    let req = make_request(type_url, payload, corr);
    let rep = rpc(
        ctx,
        sess,
        "publicGatewaySvc",
        "SendRequest",
        &[PyRep::Bytes(req.encode_proto())],
    )
    .await
    .unwrap_or_else(|e| panic!("publicGatewaySvc.SendRequest({type_url}) failed: {e:?}"));
    extract_response_proto(&rep)
}

async fn quasar_ok_raw(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_url: &str,
    payload: Vec<u8>,
    corr: &[u8],
    expected_resp_suffix: &str,
) -> Vec<u8> {
    let rsp = quasar_send(ctx, sess, type_url, payload, corr).await;
    assert_eq!(
        rsp.status_code, STATUS_OK,
        "Request {type_url} failed with status {}: {}",
        rsp.status_code, rsp.status_message
    );
    let any = rsp.payload.expect("STATUS_OK response must carry payload");
    assert!(
        any.type_url.ends_with(expected_resp_suffix),
        "Expected response type ending with '{expected_resp_suffix}', got '{}'",
        any.type_url
    );
    any.value
}

async fn quasar_ok(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_url: &str,
    payload: Vec<u8>,
    corr: &[u8],
    expected_resp_suffix: &str,
) -> DynamicQuasarMessage {
    let rsp = quasar_send(ctx, sess, type_url, payload, corr).await;
    assert_eq!(
        rsp.status_code, STATUS_OK,
        "Request {type_url} failed with status {}: {}",
        rsp.status_code, rsp.status_message
    );
    let any = rsp.payload.expect("STATUS_OK response must carry payload");
    assert!(
        any.type_url.ends_with(expected_resp_suffix),
        "Expected response type ending with '{expected_resp_suffix}', got '{}'",
        any.type_url
    );
    reg()
        .decode_dynamic(&any.type_url, &any.value)
        .unwrap_or_else(|e| panic!("decode_dynamic({}) failed: {e}", any.type_url))
}

async fn quasar_subscribe(
    ctx: &ServerContext,
    sess: &mut SessionState,
) -> tokio::sync::mpsc::Receiver<eve_proto::quasar::EvePublicNotice> {
    rpc(ctx, sess, "publicGatewaySvc", "Subscribe", &[])
        .await
        .expect("publicGatewaySvc.Subscribe");
    ctx.quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("stream_rx")
}

fn dyn_field<'a>(msg: &'a DynamicQuasarMessage, name: &str) -> Option<&'a DynamicValue> {
    let r = reg();
    let desc = r.get_message(&msg.full_name)?;
    let f = desc.fields.iter().find(|f| f.name == name)?;
    msg.get(f.number)
}

fn dyn_path<'a>(mut msg: &'a DynamicQuasarMessage, path: &str) -> Option<&'a DynamicValue> {
    let mut parts = path.split('.').peekable();
    while let Some(seg) = parts.next() {
        let val = dyn_field(msg, seg)?;
        if parts.peek().is_none() {
            return Some(val);
        }
        match val {
            DynamicValue::Message(sub) => msg = sub.as_ref(),
            _ => return None,
        }
    }
    None
}

fn dyn_u64(msg: &DynamicQuasarMessage, path: &str) -> Option<u64> {
    match dyn_path(msg, path)? {
        DynamicValue::Uint64(v) | DynamicValue::Fixed64(v) => Some(*v),
        DynamicValue::Uint32(v) | DynamicValue::Fixed32(v) => Some(*v as u64),
        DynamicValue::Int64(v) | DynamicValue::Sint64(v) | DynamicValue::Sfixed64(v) => {
            u64::try_from(*v).ok()
        }
        DynamicValue::Int32(v) | DynamicValue::Sint32(v) | DynamicValue::Sfixed32(v) => {
            u64::try_from(*v).ok()
        }
        _ => None,
    }
}

fn dyn_str<'a>(msg: &'a DynamicQuasarMessage, path: &str) -> Option<&'a str> {
    match dyn_path(msg, path)? {
        DynamicValue::String(s) => Some(s.as_str()),
        _ => None,
    }
}

fn dyn_bool(msg: &DynamicQuasarMessage, path: &str) -> Option<bool> {
    match dyn_path(msg, path)? {
        DynamicValue::Bool(b) => Some(*b),
        _ => None,
    }
}

fn dyn_list<'a>(msg: &'a DynamicQuasarMessage, field: &str) -> Vec<&'a DynamicQuasarMessage> {
    match dyn_field(msg, field) {
        Some(DynamicValue::Repeated(items)) => items
            .iter()
            .filter_map(|v| match v {
                DynamicValue::Message(m) => Some(m.as_ref()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
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

// parity: plan:Q-6
#[tokio::test]
async fn test_quasar_market_complete() {
    eve_testkit::attest(&["plan:Q-6"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = login_pilot_char(&ctx, 1001, "demo", "demo-secret", 90_000_001).await;
    ctx.advance_secs(11);

    // Seed ISK, Corporation Management skill, Tritanium stacks, corporation & corp wallet via live RPCs
    let saved_role = sess.role;
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT
        | eve_machonet::service_roles::ROLE_GML
        | eve_machonet::service_roles::ROLE_GMH;
    rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/giveskill 3363 1".into())])
        .await
        .expect("/giveskill 3363");
    rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/giveisk 5000000000".into())])
        .await
        .expect("/giveisk");
    let sell_item_char = rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/createitem 34 1000".into())])
        .await
        .expect("/createitem char trit")
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("char tritanium itemID");
    let sell_item_corp = rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/createitem 34 1000".into())])
        .await
        .expect("/createitem corp trit")
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("corp tritanium itemID");
    sess.role = saved_role;

    let corp_id = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Quasar Market Corp".into()), PyRep::String("QMC".into())],
    )
    .await
    .expect("AddCorporation")
    .dict_get("corporationID")
    .and_then(|v| v.as_i64())
    .expect("corporationID");
    rpc(
        &ctx,
        &mut sess,
        "account",
        "GiveCash",
        &[PyRep::Integer(corp_id), PyRep::Real(50_000_000.0)],
    )
    .await
    .expect("GiveCash to corp");

    // Place live character sell (4.50 ISK), corp sell (4.60 ISK), and character buy (4.20 ISK) orders
    let char_sell_oid = rpc(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(60_003_760)),
                ("typeID", PyRep::Integer(34)),
                ("itemID", PyRep::Integer(sell_item_char)),
                ("price", PyRep::Real(4.5)),
                ("quantity", PyRep::Integer(1000)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect("PlaceMultiSellOrder personal")
    .dict_get("orderID")
    .and_then(|v| v.as_i64())
    .expect("char sell orderID") as u64;

    let corp_sell_oid = rpc(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(60_003_760)),
                ("typeID", PyRep::Integer(34)),
                ("itemID", PyRep::Integer(sell_item_corp)),
                ("price", PyRep::Real(4.6)),
                ("quantity", PyRep::Integer(1000)),
            ])]),
            PyRep::Bool(true),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect("PlaceMultiSellOrder corp")
    .dict_get("orderID")
    .and_then(|v| v.as_i64())
    .expect("corp sell orderID") as u64;

    let char_buy_oid = rpc(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(60_003_760),
            PyRep::Integer(34),
            PyRep::Real(4.2),
            PyRep::Integer(500),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
            PyRep::Real(999_999.0),
        ],
    )
    .await
    .expect("PlaceBuyOrder")
    .dict_get("orderID")
    .and_then(|v| v.as_i64())
    .expect("char buy orderID") as u64;

    // 1. GetOrdersRequest
    let mut payload = Vec::new();
    encode_id_field(1, 10_000_002, &mut payload);
    encode_id_field(2, 60_003_760, &mut payload);
    encode_id_field(3, 34, &mut payload);
    let orders_rsp = quasar_ok(&ctx, &mut sess, "eve.market.orders.GetOrdersRequest", payload, b"m-01", "GetOrdersResponse").await;
    let buy_orders = dyn_list(&orders_rsp, "buy_orders");
    let sell_orders = dyn_list(&orders_rsp, "sell_orders");
    assert!(buy_orders.iter().any(|o| {
        dyn_u64(o, "id.sequential") == Some(char_buy_oid)
            && dyn_u64(o, "attributes.item_type.sequential") == Some(34)
            && dyn_u64(o, "attributes.quantity") == Some(500)
            && dyn_u64(o, "attributes.price.units") == Some(4)
            && dyn_u64(o, "attributes.price.nanos") == Some(200_000_000)
    }));
    assert!(sell_orders.iter().any(|o| {
        dyn_u64(o, "id.sequential") == Some(char_sell_oid)
            && dyn_u64(o, "attributes.quantity") == Some(1000)
            && dyn_u64(o, "attributes.price.units") == Some(4)
            && dyn_u64(o, "attributes.price.nanos") == Some(500_000_000)
    }));
    assert!(sell_orders.iter().any(|o| dyn_u64(o, "id.sequential") == Some(corp_sell_oid)));

    // 2. GetOpenForCharacterRequest
    let mut payload = Vec::new();
    encode_id_field(1, 90_000_001, &mut payload);
    let char_open = quasar_ok(&ctx, &mut sess, "eve.market.orders.GetOpenForCharacterRequest", payload.clone(), b"m-02", "GetOpenForCharacterResponse").await;
    let char_open_list = dyn_list(&char_open, "orders");
    assert!(char_open_list.iter().any(|o| dyn_u64(o, "sell.id.sequential") == Some(char_sell_oid)));
    assert!(char_open_list.iter().any(|o| dyn_u64(o, "buy.id.sequential") == Some(char_buy_oid)));

    // 3. GetHistoryForCharacterRequest
    let char_hist = quasar_ok(&ctx, &mut sess, "eve.market.orders.GetHistoryForCharacterRequest", payload, b"m-03", "GetHistoryForCharacterResponse").await;
    assert!(dyn_list(&char_hist, "orders").iter().any(|o| dyn_u64(o, "sell.id.sequential") == Some(char_sell_oid)));

    // 4. GetOpenForCorporationRequest
    let mut corp_payload = Vec::new();
    encode_id_field(1, corp_id as u64, &mut corp_payload);
    let corp_open = quasar_ok(&ctx, &mut sess, "eve.market.orders.GetOpenForCorporationRequest", corp_payload.clone(), b"m-04", "GetOpenForCorporationResponse").await;
    assert!(dyn_list(&corp_open, "orders").iter().any(|o| dyn_u64(o, "sell.id.sequential") == Some(corp_sell_oid)));

    // 5. GetHistoryForCorporationRequest
    let corp_hist = quasar_ok(&ctx, &mut sess, "eve.market.orders.GetHistoryForCorporationRequest", corp_payload, b"m-05", "GetHistoryForCorporationResponse").await;
    assert!(dyn_list(&corp_hist, "orders").iter().any(|o| dyn_u64(o, "sell.id.sequential") == Some(corp_sell_oid)));

    // 6. GetOpenInStationRequest
    let mut payload = Vec::new();
    encode_id_field(1, 60_003_760, &mut payload);
    let st_open = quasar_ok(&ctx, &mut sess, "eve.market.orders.GetOpenInStationRequest", payload, b"m-06", "GetOpenInStationResponse").await;
    let st_orders = dyn_list(&st_open, "orders");
    assert!(st_orders.iter().any(|o| dyn_u64(o, "sell.id.sequential") == Some(char_sell_oid)));
    assert!(st_orders.iter().any(|o| dyn_u64(o, "buy.id.sequential") == Some(char_buy_oid)));

    // 7. GetOpenInStructureRequest
    let mut payload = Vec::new();
    encode_id_field(1, 1_000_000_000_001, &mut payload);
    let struct_open = quasar_ok(&ctx, &mut sess, "eve.market.orders.GetOpenInStructureRequest", payload, b"m-07", "GetOpenInStructureResponse").await;
    assert!(dyn_list(&struct_open, "orders").is_empty());

    // 8. GetPublicOpenInRegionRequest
    let mut payload = Vec::new();
    encode_id_field(1, 10_000_002, &mut payload);
    let reg_open = quasar_ok(&ctx, &mut sess, "eve.market.orders.GetPublicOpenInRegionRequest", payload.clone(), b"m-08", "GetPublicOpenInRegionResponse").await;
    assert!(dyn_list(&reg_open, "orders").iter().any(|o| dyn_u64(o, "sell.id.sequential") == Some(corp_sell_oid)));

    // 9. buyorder.GetAllPublicOpenInRegionRequest
    let reg_buys = quasar_ok(&ctx, &mut sess, "eve.market.orders.buyorder.GetAllPublicOpenInRegionRequest", payload.clone(), b"m-09", "buyorder.GetAllPublicOpenInRegionResponse").await;
    assert!(dyn_list(&reg_buys, "orders").iter().any(|o| dyn_u64(o, "id.sequential") == Some(char_buy_oid)));

    // 10. sellorder.GetAllPublicOpenInRegionRequest
    let reg_sells = quasar_ok(&ctx, &mut sess, "eve.market.orders.sellorder.GetAllPublicOpenInRegionRequest", payload, b"m-10", "sellorder.GetAllPublicOpenInRegionResponse").await;
    assert!(dyn_list(&reg_sells, "orders").iter().any(|o| dyn_u64(o, "id.sequential") == Some(char_sell_oid)));

    // 11. GetPriceHistoryRequest
    let mut payload = Vec::new();
    encode_id_field(1, 10_000_002, &mut payload);
    encode_id_field(3, 34, &mut payload);
    let ph = quasar_ok(&ctx, &mut sess, "eve.market.orders.GetPriceHistoryRequest", payload, b"m-11", "GetPriceHistoryResponse").await;
    let ph_entries = dyn_list(&ph, "entries");
    assert_eq!(ph_entries.len(), 1);
    assert_eq!(dyn_u64(ph_entries[0], "order_count"), Some(5));
    assert!(dyn_u64(ph_entries[0], "average.units").unwrap_or(0) > 0);

    // 12. GetTypesWithActiveOrdersInRegionRequest
    let mut payload = Vec::new();
    encode_id_field(1, 10_000_002, &mut payload);
    let active_types = quasar_ok(&ctx, &mut sess, "eve.market.orders.GetTypesWithActiveOrdersInRegionRequest", payload, b"m-12", "GetTypesWithActiveOrdersInRegionResponse").await;
    assert!(dyn_list(&active_types, "types").iter().any(|t| dyn_u64(t, "sequential") == Some(34)));

    // 13. character.sellorder.GetRequest
    let mut payload = Vec::new();
    encode_id_field(1, char_sell_oid, &mut payload);
    let char_sell_get = quasar_ok(&ctx, &mut sess, "eve.market.character.sellorder.GetRequest", payload, b"m-13", "character.sellorder.GetResponse").await;
    assert_eq!(dyn_u64(&char_sell_get, "order.character.sequential"), Some(90_000_001));
    assert_eq!(dyn_u64(&char_sell_get, "order.item.item_type.sequential"), Some(34));
    assert_eq!(dyn_u64(&char_sell_get, "order.item.quantity"), Some(1000));

    // 14. corporation.sellorder.GetRequest
    let mut corp_order_payload = Vec::new();
    encode_id_field(1, corp_sell_oid, &mut corp_order_payload);
    let corp_sell_get = quasar_ok(&ctx, &mut sess, "eve.market.corporation.sellorder.GetRequest", corp_order_payload, b"m-14", "corporation.sellorder.GetResponse").await;
    assert_eq!(dyn_u64(&corp_sell_get, "order.corporation.sequential"), Some(corp_id as u64));
    assert_eq!(dyn_u64(&corp_sell_get, "order.item.item_type.sequential"), Some(34));
    assert_eq!(dyn_u64(&corp_sell_get, "order.item.quantity"), Some(1000));

    // 15. price.history.GetForRegionAndTypeRequest
    let mut payload = Vec::new();
    encode_id_field(1, 10_000_002, &mut payload);
    encode_id_field(2, 34, &mut payload);
    let reg_type_hist = quasar_ok(&ctx, &mut sess, "eve.market.price.history.GetForRegionAndTypeRequest", payload.clone(), b"m-15", "GetForRegionAndTypeResponse").await;
    assert!(dyn_u64(dyn_list(&reg_type_hist, "day_statistics")[0], "average.units").unwrap_or(0) > 0);

    // 16. region.GetPriceHistoryForTypeRequest
    let reg_hist_type = quasar_ok(&ctx, &mut sess, "eve.market.region.GetPriceHistoryForTypeRequest", payload, b"m-16", "GetPriceHistoryForTypeResponse").await;
    assert!(dyn_u64(dyn_list(&reg_hist_type, "day_statistics")[0], "average.units").unwrap_or(0) > 0);

    // 17. price.GetForAllTypesRequest
    let all_prices = quasar_ok(&ctx, &mut sess, "eve.market.price.GetForAllTypesRequest", Vec::new(), b"m-17", "GetForAllTypesResponse").await;
    assert!(dyn_list(&all_prices, "types").iter().any(|t| {
        dyn_u64(t, "id.sequential") == Some(34) && dyn_u64(t, "average_price.units").unwrap_or(0) > 0
    }));

    // 18. Market notice emission and subscriber reception
    let mut stream_rx = quasar_subscribe(&ctx, &mut sess).await;
    while stream_rx.try_recv().is_ok() {}
    let delivered = publish_market_new_entry_notice(&ctx.quasar_gateway, 30_000_142, ctx.now_unix())
        .expect("publish");
    assert_eq!(delivered, 1);
    let received = stream_rx.try_recv().expect("receive market notice");
    let notice_any = received.payload.expect("notice payload");
    assert!(notice_any.type_url.ends_with("NewEntryNotice"));
    let notice_dyn = reg().decode_dynamic(&notice_any.type_url, &notice_any.value).expect("decode NewEntryNotice");
    assert_eq!(notice_dyn.full_name, "eve_public.market.entry.NewEntryNotice");
}

// parity: plan:Q-6
#[tokio::test]
async fn test_quasar_inventory_asset_container_blueprint_ore() {
    eve_testkit::attest(&["plan:Q-6"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = login_pilot_char(&ctx, 1001, "demo", "demo-secret", 90_000_001).await;

    // Create a live Rifter (type 587, Category 6 Ship) in station hangar via SlashCmd
    let saved_role = sess.role;
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT
        | eve_machonet::service_roles::ROLE_GML
        | eve_machonet::service_roles::ROLE_GMH;
    let rifter_id = rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/createitem 587 1".into())])
        .await
        .expect("/createitem Rifter")
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("Rifter itemID") as u64;
    sess.role = saved_role;

    // 1. Inventory Category Get & GetAll + negative NotFound
    let mut payload = Vec::new();
    encode_id_field(1, 6, &mut payload); // Category 6: Ship
    let cat_get = quasar_ok(&ctx, &mut sess, "eve.inventory.category.GetRequest", payload, b"inv-01", "category.GetResponse").await;
    assert_eq!(dyn_str(&cat_get, "attributes.name"), Some("Ship"));
    assert_eq!(dyn_bool(&cat_get, "attributes.published"), Some(true));

    let mut missing_cat = Vec::new();
    encode_id_field(1, 999_999_999, &mut missing_cat);
    let missing_rsp = quasar_send(&ctx, &mut sess, "eve.inventory.category.GetRequest", missing_cat, b"inv-01b").await;
    assert_eq!(missing_rsp.status_code, STATUS_NOT_FOUND);

    let cat_all = quasar_ok(&ctx, &mut sess, "eve.inventory.category.GetAllRequest", Vec::new(), b"inv-02", "category.GetAllResponse").await;
    assert!(dyn_list(&cat_all, "categories").iter().any(|c| dyn_u64(c, "sequential") == Some(6)));

    // 2. Inventory Group Get, GetAll & GetGroupsInCategory
    let mut payload = Vec::new();
    encode_id_field(1, 25, &mut payload); // Group 25: Frigate
    let grp_get = quasar_ok(&ctx, &mut sess, "eve.inventory.group.GetRequest", payload, b"inv-03", "group.GetResponse").await;
    assert_eq!(dyn_str(&grp_get, "attributes.name"), Some("Frigate"));
    assert_eq!(dyn_u64(&grp_get, "attributes.category.sequential"), Some(6));
    assert_eq!(dyn_bool(&grp_get, "attributes.published"), Some(true));

    let grp_all = quasar_ok(&ctx, &mut sess, "eve.inventory.group.GetAllRequest", Vec::new(), b"inv-04", "group.GetAllResponse").await;
    assert!(!dyn_list(&grp_all, "groups").is_empty());

    let mut cat_payload = Vec::new();
    encode_id_field(1, 6, &mut cat_payload);
    let grp_in_cat = quasar_ok(&ctx, &mut sess, "eve.inventory.group.GetGroupsInCategoryRequest", cat_payload, b"inv-05", "GetGroupsInCategoryResponse").await;
    assert!(dyn_list(&grp_in_cat, "groups").iter().any(|g| dyn_u64(g, "sequential") == Some(25)));

    // 3. Inventory Market Group Get & GetAll
    let mut payload = Vec::new();
    encode_id_field(1, 4, &mut payload); // MarketGroup 4: Ships
    let mg_get = quasar_ok(&ctx, &mut sess, "eve.inventory.market_group.GetRequest", payload, b"inv-06", "market_group.GetResponse").await;
    assert_eq!(dyn_str(&mg_get, "group.name"), Some("Ships"));
    assert_eq!(dyn_bool(&mg_get, "group.no_parent_group"), Some(true));

    let mg_all = quasar_ok(&ctx, &mut sess, "eve.inventory.market_group.GetAllRequest", Vec::new(), b"inv-07", "market_group.GetAllResponse").await;
    assert!(dyn_list(&mg_all, "groups").iter().any(|g| dyn_u64(g, "sequential") == Some(4)));

    // 4. Inventory genericitemtype Get, GetAll, TypesInGroup, MarketGroupInfo & TypeValuation
    let mut payload = Vec::new();
    encode_id_field(1, 34, &mut payload); // Type 34: Tritanium
    let type_get = quasar_ok(&ctx, &mut sess, "eve.inventory.genericitemtype.GetRequest", payload.clone(), b"inv-08", "genericitemtype.GetResponse").await;
    assert_eq!(dyn_str(&type_get, "type_info.name"), Some("Tritanium"));
    assert!(dyn_u64(&type_get, "type_info.group.sequential").unwrap_or(0) > 0);

    let type_all = quasar_ok(&ctx, &mut sess, "eve.inventory.genericitemtype.GetAllRequest", Vec::new(), b"inv-09", "genericitemtype.GetAllResponse").await;
    assert!(!dyn_list(&type_all, "types").is_empty());

    let mut grp_payload = Vec::new();
    encode_id_field(1, 25, &mut grp_payload);
    let types_in_grp = quasar_ok(&ctx, &mut sess, "eve.inventory.genericitemtype.GetTypesInGroupRequest", grp_payload, b"inv-10", "GetTypesInGroupResponse").await;
    assert!(dyn_list(&types_in_grp, "types").iter().any(|t| dyn_u64(t, "sequential") == Some(587)));

    let mut mg_payload = Vec::new();
    encode_id_field(4, 4, &mut mg_payload);
    let mg_types = quasar_ok(&ctx, &mut sess, "eve.inventory.genericitemtype.GetTypeInfoByMarketGroupRequest", mg_payload, b"inv-11", "GetTypeInfoByMarketGroupResponse").await;
    for entry in dyn_list(&mg_types, "types") {
        assert!(dyn_u64(entry, "id.sequential").unwrap_or(0) > 0);
        assert!(dyn_str(entry, "type.name").is_some_and(|n| !n.is_empty()));
    }

    let val_rsp = quasar_ok(&ctx, &mut sess, "eve.inventory.genericitemtype.TypeValuationRequest", payload.clone(), b"inv-12", "TypeValuationResponse").await;
    assert_eq!(dyn_u64(&val_rsp, "type_value.units"), Some(1));
    assert_eq!(dyn_u64(&val_rsp, "valuation_timestamp.seconds"), Some(1_760_000_000));

    // 5. genericname & genericitem Search
    let name_rsp = quasar_ok(&ctx, &mut sess, "eve.inventory.genericname.GetNameByIDRequest", payload, b"inv-13", "GetNameByIDResponse").await;
    assert_eq!(dyn_str(&name_rsp, "name"), Some("Tritanium"));

    let mut name_arr = Vec::new();
    encode_id_field(1, 34, &mut name_arr);
    let names_cats = quasar_ok(&ctx, &mut sess, "eve.inventory.genericname.GetNamesAndCategoriesByIDsRequest", name_arr, b"inv-14", "GetNamesAndCategoriesByIDsResponse").await;
    assert!(dyn_list(&names_cats, "items_name_and_category").iter().any(|e| {
        dyn_u64(e, "id.sequential") == Some(34) && dyn_str(e, "name") == Some("Tritanium")
    }));

    let search_rsp = quasar_ok(&ctx, &mut sess, "eve.inventory.genericitem.SearchRequest", Vec::new(), b"inv-15", "genericitem.SearchResponse").await;
    assert!(!dyn_list(&search_rsp, "generic_items").is_empty());

    // 6. assetholding operations (Hold, HoldOfType, SpawnOfType)
    let mut op_payload = Vec::new();
    encode_uuid_field(1, b"op-inv-0001", &mut op_payload);
    let hold_rsp = quasar_ok(&ctx, &mut sess, "eve.inventory.assetholding.operation.api.HoldItemsRequest", op_payload.clone(), b"inv-16", "HoldItemsResponse").await;
    assert_eq!(dyn_u64(&hold_rsp, "container.sequential"), Some(999_001));
    assert_eq!(dyn_u64(&hold_rsp, "source_location_used.item.sequential"), Some(60_003_760));
    assert!(!dyn_list(&hold_rsp, "items").is_empty());

    let hold_type_rsp = quasar_ok(&ctx, &mut sess, "eve.inventory.assetholding.operation.api.HoldItemsOfTypeRequest", op_payload.clone(), b"inv-17", "HoldItemsOfTypeResponse").await;
    assert_eq!(dyn_u64(&hold_type_rsp, "container.sequential"), Some(999_002));
    assert_eq!(dyn_u64(&hold_type_rsp, "source_location_used.item.sequential"), Some(60_003_760));

    let spawn_rsp = quasar_ok(&ctx, &mut sess, "eve.inventory.assetholding.operation.api.SpawnItemsOfTypeRequest", op_payload, b"inv-18", "SpawnItemsOfTypeResponse").await;
    assert_eq!(dyn_u64(&spawn_rsp, "location.item.sequential"), Some(60_003_760));
    assert!(!dyn_list(&spawn_rsp, "items").is_empty());

    // 7. Asset: GetAssetsItemsRequest & GetShipAssetsRequest
    let mut char_payload = Vec::new();
    encode_id_field(1, 90_000_001, &mut char_payload);
    let assets_rsp = quasar_ok(&ctx, &mut sess, "eve.asset.GetAssetsItemsRequest", char_payload.clone(), b"inv-19", "GetAssetItemsResponse").await;
    assert!(dyn_list(&assets_rsp, "assets").iter().any(|a| {
        dyn_u64(a, "asset.item_id.sequential") == Some(rifter_id)
            && dyn_u64(a, "asset.item_attributes.item_type.sequential") == Some(587)
            && dyn_u64(a, "asset.item_attributes.quantity") == Some(1)
    }));

    let ship_raw = quasar_ok_raw(&ctx, &mut sess, "eve.asset.GetShipAssetsRequest", char_payload, b"inv-20", "GetShipAssetsResponse").await;
    let ship_wm = WireMessage::decode(&ship_raw).expect("decode GetShipAssetsResponse");
    let has_rifter = ship_wm.fields.iter().filter(|f| f.field_number == 1).any(|f| {
        let entry = WireMessage::decode(f.value.as_bytes().unwrap_or(&[])).expect("entry");
        let ship_data = crate::quasar_goal_tale_notices::sub(&entry, 1);
        crate::quasar_goal_tale_notices::varint(&crate::quasar_goal_tale_notices::sub(&ship_data, 1), 1) == Some(rifter_id)
            && crate::quasar_goal_tale_notices::varint(&crate::quasar_goal_tale_notices::sub(&ship_data, 2), 1) == Some(587)
            && crate::quasar_goal_tale_notices::varint(&crate::quasar_goal_tale_notices::sub(&entry, 2), 1) == Some(60_003_760)
    });
    assert!(has_rifter, "GetShipAssetsResponse must contain live created Rifter {rifter_id}");
}

// parity: plan:Q-6
#[tokio::test]
async fn test_quasar_assetholding_entitlements() {
    eve_testkit::attest(&["plan:Q-6"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = login_pilot_char(&ctx, 1001, "demo", "demo-secret", 90_000_001).await;
    let mut gm_sess = login_pilot_char(&ctx, 1002, "other", "other-secret", 90_000_002).await;
    gm_sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;

    let mut stream_rx = quasar_subscribe(&ctx, &mut sess).await;

    // 1. Asset API (Spawn, Get, eve_public.Get, Hold, Create)
    let unauth_spawn = quasar_send(&ctx, &mut sess, "eve.assetholding.asset.api.SpawnRequest", Vec::new(), b"ah-04").await;
    assert_eq!(unauth_spawn.status_code, STATUS_FORBIDDEN, "SpawnRequest without GM role must return 403");

    let (mut stack, mut stacks, mut spawn_payload) = (Vec::new(), Vec::new(), Vec::new());
    encode_id_field(1, 34, &mut stack);
    encode_varint_field(2, 50, &mut stack);
    encode_bytes_field(1, &stack, &mut stacks);
    eve_proto::quasar::encode_string_field(2, "Econ Asset", &mut spawn_payload);
    encode_bytes_field(6, &stacks, &mut spawn_payload);
    let _spawn_rsp = quasar_ok(&ctx, &mut gm_sess, "eve.assetholding.asset.api.SpawnRequest", spawn_payload, b"ah-04b", "SpawnResponse").await;

    let mut asset_uuid = [0u8; 16];
    asset_uuid[0..8].copy_from_slice(b"ASSETHLD");
    asset_uuid[8..16].copy_from_slice(&1u64.to_be_bytes());
    let mut asset_payload = Vec::new();
    encode_uuid_field(1, &asset_uuid, &mut asset_payload);

    let asset_get = quasar_ok(&ctx, &mut sess, "eve.assetholding.asset.api.GetRequest", asset_payload.clone(), b"ah-01", "asset.api.GetResponse").await;
    assert_eq!(dyn_str(&asset_get, "asset.internal_name"), Some("Econ Asset"));
    assert_eq!(
        dyn_u64(&asset_get, "asset.benefactor.character.sequential"),
        Some(90_000_002)
    );
    let asset_stacks = match dyn_path(&asset_get, "asset.unit.unit_inventory_stack.stacks") {
        Some(DynamicValue::Repeated(v)) => v
            .iter()
            .filter_map(|x| match x {
                DynamicValue::Message(m) => Some(m.as_ref()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    assert_eq!(asset_stacks.len(), 1);
    assert_eq!(dyn_u64(asset_stacks[0], "type.sequential"), Some(34));
    assert_eq!(dyn_u64(asset_stacks[0], "quantity"), Some(50));

    let pub_asset_get = quasar_ok(&ctx, &mut sess, "eve_public.assetholding.asset.api.GetRequest", asset_payload, b"ah-02", "asset.api.GetResponse").await;
    assert_eq!(dyn_bool(&pub_asset_get, "asset.capacity.infinite"), Some(true));

    let (mut skin_hex_msg, mut skin_unit, mut skin_hp, mut hold_payload) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    eve_proto::quasar::encode_string_field(1, "skin_econ_01", &mut skin_hex_msg);
    encode_bytes_field(1, &skin_hex_msg, &mut skin_unit);
    encode_bytes_field(1, &skin_unit, &mut skin_hp);
    encode_varint_field(2, 2, &mut skin_hp);
    encode_bytes_field(11, &skin_hp, &mut hold_payload);
    let _hold_rsp = quasar_ok(&ctx, &mut gm_sess, "eve.assetholding.asset.api.HoldRequest", hold_payload, b"ah-03", "HoldResponse").await;

    let (mut create_unit, mut create_payload) = (Vec::new(), Vec::new());
    encode_bytes_field(10, &skin_unit, &mut create_unit);
    encode_bytes_field(4, &create_unit, &mut create_payload);
    encode_varint_field(6, 5, &mut create_payload);
    let _create_rsp = quasar_ok(&ctx, &mut gm_sess, "eve.assetholding.asset.api.CreateRequest", create_payload, b"ah-05", "CreateResponse").await;

    // 2. Entitlement API (DirectDelivery, Get, Redeem, Delete, GetUnredeemed, Admin Expire & AutoRedeem)
    let mut char_id_msg = Vec::new();
    encode_varint_field(1, 90_000_001, &mut char_id_msg);
    let mut holder_msg = Vec::new();
    encode_bytes_field(1, &char_id_msg, &mut holder_msg);

    let make_ent_id = |uuid: &[u8; 16], grp_uuid: &[u8; 16]| -> Vec<u8> {
        let (mut a_msg, mut g_msg, mut e_id) = (Vec::new(), Vec::new(), Vec::new());
        encode_bytes_field(1, uuid, &mut a_msg);
        eve_proto::quasar::encode_string_field(1, "econgroup", &mut g_msg);
        encode_bytes_field(2, grp_uuid, &mut g_msg);
        encode_bytes_field(1, &holder_msg, &mut e_id);
        encode_bytes_field(2, &a_msg, &mut e_id);
        encode_bytes_field(3, &g_msg, &mut e_id);
        e_id
    };

    assert_ne!(
        quasar_send(&ctx, &mut sess, "eve.assetholding.entitlement.api.DirectDeliveryRequest", Vec::new(), b"ah-10").await.status_code,
        STATUS_OK,
        "empty DirectDelivery without GM role must fail"
    );

    let missing_uuid = *b"MISSING_ASSET_16";
    let mut missing_ent_payload = Vec::new();
    encode_bytes_field(1, &make_ent_id(&missing_uuid, b"GROUP_MISSING_16"), &mut missing_ent_payload);
    assert_eq!(
        quasar_send(&ctx, &mut sess, "eve_public.assetholding.entitlement.api.GetRequest", missing_ent_payload.clone(), b"ah-07").await.status_code,
        STATUS_NOT_FOUND,
        "unknown entitlement must return 404"
    );
    assert_ne!(
        quasar_send(&ctx, &mut sess, "eve_public.assetholding.entitlement.api.RedeemRequest", missing_ent_payload, b"ah-09").await.status_code,
        STATUS_OK,
        "unknown entitlement cannot be redeemed"
    );

    let mut dd_payload = Vec::new();
    for g in [b"GROUP_ECON_00001", b"GROUP_ECON_00002", b"GROUP_ECON_00003", b"GROUP_ECON_00004"] {
        let mut entry = Vec::new();
        encode_bytes_field(2, &make_ent_id(&asset_uuid, g), &mut entry);
        encode_varint_field(10, 1, &mut entry);
        encode_bytes_field(2, &entry, &mut dd_payload);
    }
    quasar_ok(&ctx, &mut gm_sess, "eve.assetholding.entitlement.api.DirectDeliveryRequest", dd_payload, b"ah-10b", "DirectDeliveryResponse").await;

    let ent_payload_for = |g: &[u8; 16]| {
        let mut p = Vec::new();
        encode_bytes_field(1, &make_ent_id(&asset_uuid, g), &mut p);
        p
    };
    let ent1 = ent_payload_for(b"GROUP_ECON_00001");
    let ent2 = ent_payload_for(b"GROUP_ECON_00002");
    let ent3 = ent_payload_for(b"GROUP_ECON_00003");
    let ent4 = ent_payload_for(b"GROUP_ECON_00004");

    let ent1_get = quasar_ok(&ctx, &mut sess, "eve.assetholding.entitlement.api.GetRequest", ent1.clone(), b"ah-06", "entitlement.api.GetResponse").await;
    assert_eq!(dyn_u64(&ent1_get, "entitlement.quantity.total"), Some(1));
    assert_eq!(dyn_u64(&ent1_get, "entitlement.quantity.redeemed"), Some(0));

    let mut unredeemed_payload = Vec::new();
    encode_bytes_field(1, &holder_msg, &mut unredeemed_payload);
    let unred_before = quasar_ok(&ctx, &mut sess, "eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest", unredeemed_payload.clone(), b"ah-12", "GetUnredeemedByHolderAndGroupNameResponse").await;
    let ents_before = dyn_list(&unred_before, "entitlements");
    assert_eq!(ents_before.len(), 4);
    assert!(ents_before.iter().all(|e| {
        dyn_u64(e, "quantity.total") == Some(1)
            && dyn_u64(e, "quantity.redeemed") == Some(0)
            && dyn_str(e, "entitlement.group_by.name") == Some("econgroup")
    }));

    quasar_ok(&ctx, &mut sess, "eve.assetholding.entitlement.api.DeleteRequest", ent2, b"ah-11", "DeleteResponse").await;

    // 3. Admin Entitlement Handlers
    quasar_ok(&ctx, &mut gm_sess, "eve.assetholding.entitlement.api.admin.RedeemRequest", ent1, b"ah-13", "RedeemResponse").await;
    quasar_ok(&ctx, &mut gm_sess, "eve.assetholding.entitlement.api.admin.ExpireRequest", ent3, b"ah-14", "ExpireResponse").await;
    quasar_ok(&ctx, &mut gm_sess, "eve_public.assetholding.entitlement.api.admin.ExpireRequest", ent4, b"ah-17", "ExpireResponse").await;

    let unred_after = quasar_ok(&ctx, &mut sess, "eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest", unredeemed_payload, b"ah-12b", "GetUnredeemedByHolderAndGroupNameResponse").await;
    assert_eq!(dyn_list(&unred_after, "entitlements").len(), 0);

    for (url, corr) in [
        ("eve.assetholding.entitlement.api.admin.AutoRedeemRequest", &b"ah-15"[..]),
        ("eve.assetholding.entitlement.api.admin.RedeliverStaleEntitlementsRequest", &b"ah-16"[..]),
        ("eve_public.assetholding.entitlement.api.admin.AutoRedeemRequest", &b"ah-18"[..]),
    ] {
        quasar_ok(&ctx, &mut gm_sess, url, Vec::new(), corr, "Response").await;
    }

    // 4. Notices fanout across character scope: Granted, Redeemed, Deleted (from Delete/Expire), and CapacityClaimed
    let mut seen_urls = Vec::new();
    while let Ok(msg) = stream_rx.try_recv() {
        if let Some(p) = msg.payload {
            let decoded = reg().decode_dynamic(&p.type_url, &p.value).expect("decode notice");
            assert!(!decoded.full_name.is_empty());
            seen_urls.push(p.type_url);
        }
    }
    assert!(seen_urls.iter().any(|u| u.ends_with("GrantedNotice")));
    assert!(seen_urls.iter().any(|u| u.ends_with("RedeemedNotice")));
    assert!(seen_urls.iter().any(|u| u.ends_with("DeletedNotice")));

    let d3 = publish_asset_capacity_claimed_notice(&ctx.quasar_gateway, 90_000_001, 501, 10, 5, ctx.now_unix()).expect("n3");
    assert_eq!(d3, 1);
    let msg3 = stream_rx.try_recv().expect("recv msg3");
    let p3 = msg3.payload.expect("cap notice payload");
    assert!(p3.type_url.ends_with("FiniteCapacityClaimedNotice"));
    let _cap_dyn = reg().decode_dynamic(&p3.type_url, &p3.value).expect("decode FiniteCapacityClaimedNotice");

    let d4 = publish_asset_deleted_notice(&ctx.quasar_gateway, 90_000_001, 501, ctx.now_unix()).expect("n4");
    assert_eq!(d4, 1);
    let msg4 = stream_rx.try_recv().expect("recv msg4");
    let p4 = msg4.payload.expect("del notice payload");
    assert!(p4.type_url.ends_with("DeletedNotice"));
    let _del_dyn = reg().decode_dynamic(&p4.type_url, &p4.value).expect("decode DeletedNotice");
}



#[allow(dead_code, unused_imports)]
pub(crate) mod quasar_economy_ext {
    use super::*;

//  Complete Quasar Economy & Industry integration test suite — Part 2 (`plan:Q-6`).
//
//  Covers:
//  - Accounting, Loyalty Points & Contracts
//  - Industry, Planet Interaction, Skyhook, Moon & Asteroid Belt
//  - P1 Track A Regressions (Issues #20, #21, #22)

// parity: plan:Q-6
#[tokio::test]
async fn test_quasar_accounting_loyalty_contracts() {
    eve_testkit::attest(&["plan:Q-6"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = login_pilot_char(&ctx, 1001, "demo", "demo-secret", 90_000_001).await;
    let mut other_sess = login_pilot_char(&ctx, 1002, "other", "other-secret", 90_000_002).await;
    ctx.advance_secs(11);

    // Seed skills, ISK, item, corporation, corp donation journal entry, and public contract via live RPCs
    let saved_role = sess.role;
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT
        | eve_machonet::service_roles::ROLE_GML
        | eve_machonet::service_roles::ROLE_GMH;
    rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/giveskill 3363 1".into())])
        .await
        .expect("/giveskill 3363");
    rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/giveisk 5000000000".into())])
        .await
        .expect("/giveisk");
    let contract_item_id = rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/createitem 34 100".into())])
        .await
        .expect("/createitem trit")
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("contract itemID");
    sess.role = saved_role;

    let corp_id = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Quasar Accounting Corp".into()), PyRep::String("QAC".into())],
    )
    .await
    .expect("AddCorporation")
    .dict_get("corporationID")
    .and_then(|v| v.as_i64())
    .expect("corporationID");

    rpc(
        &ctx,
        &mut sess,
        "account",
        "GiveCash",
        &[PyRep::Integer(corp_id), PyRep::Real(10_000_000.0)],
    )
    .await
    .expect("GiveCash to corp");

    let created_contract = rpc(
        &ctx,
        &mut sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("type", PyRep::String("ItemExchange".into())),
            ("availability", PyRep::String("Public".into())),
            ("startStationID", PyRep::Integer(60_003_760)),
            ("price", PyRep::Real(1_250_000.0)),
            ("itemList", PyRep::Integer(contract_item_id)),
        ])],
    )
    .await
    .expect("CreateContract");
    let contract_id = created_contract
        .as_i64()
        .or_else(|| match &created_contract {
            PyRep::List(items) | PyRep::Tuple(items) => items.first().and_then(|v| v.as_i64()),
            _ => None,
        })
        .or_else(|| created_contract.dict_get("contractID").and_then(|v| v.as_i64()))
        .expect("contractID") as u64;

    // 1. Character & Corporation Journal Entries (+ cross-char / cross-corp 403)
    let mut payload = Vec::new();
    encode_id_field(1, 90_000_001, &mut payload);
    let char_journal = quasar_ok(&ctx, &mut sess, "eve.accounting.character.journal.GetCharacterEntriesRequest", payload.clone(), b"acc-01", "GetCharacterEntriesResponse").await;
    let char_entries = dyn_list(&char_journal, "entries");
    assert!(!char_entries.is_empty());
    assert!(char_entries.iter().any(|e| {
        dyn_u64(e, "id.transaction").unwrap_or(0) > 0
            && dyn_u64(e, "attributes.account_balance.units").unwrap_or(0) > 0
            && dyn_str(e, "attributes.description").is_some_and(|d| d.contains("Player donation"))
    }));
    assert_eq!(
        quasar_send(&ctx, &mut other_sess, "eve.accounting.character.journal.GetCharacterEntriesRequest", payload, b"acc-01b").await.status_code,
        STATUS_FORBIDDEN
    );

    let mut corp_req = Vec::new();
    encode_id_field(1, corp_id as u64, &mut corp_req);
    let corp_journal = quasar_ok(&ctx, &mut sess, "eve.accounting.corporation.journal.GetCorporationEntriesRequest", corp_req.clone(), b"acc-02", "GetCorporationEntriesResponse").await;
    let corp_entries = dyn_list(&corp_journal, "entries");
    assert!(!corp_entries.is_empty());
    assert!(corp_entries.iter().any(|e| {
        dyn_u64(e, "id.transaction").unwrap_or(0) > 0
            && dyn_u64(e, "attributes.amount.units") == Some(10_000_000)
            && dyn_u64(e, "attributes.account_balance.units").unwrap_or(0) >= 10_000_000
    }));
    assert_eq!(
        quasar_send(&ctx, &mut other_sess, "eve.accounting.corporation.journal.GetCorporationEntriesRequest", corp_req, b"acc-02b").await.status_code,
        STATUS_FORBIDDEN
    );

    // 2. Accounting Entry Types
    let entry_types_rsp = quasar_ok(&ctx, &mut sess, "eve.accounting.entrytype.GetAllRequest", Vec::new(), b"acc-03", "entrytype.GetAllResponse").await;
    let et_list = dyn_list(&entry_types_rsp, "entry_types");
    assert_eq!(et_list.len(), 4);
    assert!(et_list.iter().any(|et| {
        dyn_u64(et, "id.sequential") == Some(1) && dyn_str(et, "attributes.name") == Some("Player Trading")
    }));
    assert!(et_list.iter().any(|et| {
        dyn_u64(et, "id.sequential") == Some(42) && dyn_str(et, "attributes.name") == Some("Industry Job Fee")
    }));

    // 3. Loyalty Points Wallet assetholding operations (Deposit & Withdraw + unauth 403)
    let mut lp_payload = Vec::new();
    let lp_uuid = [0x42u8; 16];
    encode_uuid_field(1, &lp_uuid, &mut lp_payload);
    let mut wallet_bytes = Vec::new();
    encode_id_field(1, 90_000_001, &mut wallet_bytes);
    encode_id_field(3, 1_000_009, &mut wallet_bytes);
    let mut curr_bytes = Vec::new();
    curr_bytes.push(0x08);
    curr_bytes.push(100);
    let mut attr_bytes = Vec::new();
    attr_bytes.push(0x0a);
    attr_bytes.push(wallet_bytes.len() as u8);
    attr_bytes.extend_from_slice(&wallet_bytes);
    attr_bytes.push(0x12);
    attr_bytes.push(curr_bytes.len() as u8);
    attr_bytes.extend_from_slice(&curr_bytes);
    lp_payload.push(0x12);
    lp_payload.push(attr_bytes.len() as u8);
    lp_payload.extend_from_slice(&attr_bytes);

    let mut unauth_sess = SessionState::new(1099);
    assert_eq!(
        quasar_send(&ctx, &mut unauth_sess, "eve.loyaltypoints.assetholding.operation.api.DepositRequest", lp_payload.clone(), b"lp-00").await.status_code,
        eve_server::quasar_gateway::STATUS_UNAUTHORIZED
    );
    assert_eq!(
        quasar_send(&ctx, &mut other_sess, "eve.loyaltypoints.assetholding.operation.api.DepositRequest", lp_payload.clone(), b"lp-00b").await.status_code,
        STATUS_FORBIDDEN
    );
    let saved_lp_role = sess.role;
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    quasar_ok(&ctx, &mut sess, "eve.loyaltypoints.assetholding.operation.api.DepositRequest", lp_payload.clone(), b"lp-01", "DepositResponse").await;
    quasar_ok(&ctx, &mut sess, "eve.loyaltypoints.assetholding.operation.api.WithdrawRequest", lp_payload, b"lp-02", "WithdrawResponse").await;
    sess.role = saved_lp_role;

    // 4. Contract Item Exchange (real live contract created via contractProxy.CreateContract)
    let mut contract_payload = Vec::new();
    encode_id_field(1, contract_id, &mut contract_payload);
    let contract_rsp = quasar_ok(&ctx, &mut sess, "eve.contract.item_exchange.GetRequest", contract_payload, b"cnt-01", "item_exchange.GetResponse").await;
    assert_eq!(dyn_u64(&contract_rsp, "contract.issuer.character.sequential"), Some(90_000_001));
    assert_eq!(dyn_bool(&contract_rsp, "contract.available_to_public"), Some(true));
    assert_eq!(dyn_u64(&contract_rsp, "contract.price.units"), Some(1_250_000));
}

// parity: plan:Q-6
#[tokio::test]
async fn test_quasar_industry_planetary_moon_asteroidbelt() {
    eve_testkit::attest(&["plan:Q-6"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = login_pilot_char(&ctx, 1001, "demo", "demo-secret", 90_000_001).await;

    // 1. Industry (jobs, blueprints, facilities, cost indices)
    let mut char_payload = Vec::new();
    encode_id_field(1, 90_000_001, &mut char_payload);
    let jobs_get = quasar_ok(&ctx, &mut sess, "eve.character.industry.job.GetRequest", char_payload.clone(), b"ind-01", "industry.job.GetResponse").await;
    assert!(dyn_list(&jobs_get, "jobs").iter().any(|j| dyn_u64(j, "id.sequential") == Some(80001)));

    let jobs_active = quasar_ok(&ctx, &mut sess, "eve.character.industry.job.GetActiveRequest", char_payload.clone(), b"ind-01a", "GetActiveResponse").await;
    assert!(dyn_list(&jobs_active, "jobs").iter().any(|j| dyn_u64(j, "id.sequential") == Some(80001)));

    let jobs_done = quasar_ok(&ctx, &mut sess, "eve.character.industry.job.GetCompletedRequest", char_payload.clone(), b"ind-01c", "GetCompletedResponse").await;
    assert!(dyn_list(&jobs_done, "jobs").iter().any(|j| dyn_u64(j, "id.sequential") == Some(80001)));

    let bps = quasar_ok(&ctx, &mut sess, "eve.character.industry.blueprints.GetRequest", char_payload, b"ind-01b", "blueprints.GetResponse").await;
    assert!(dyn_list(&bps, "blueprints").is_empty());

    let fac = quasar_ok(&ctx, &mut sess, "eve.industry.facility.GetRequest", Vec::new(), b"ind-02", "facility.GetResponse").await;
    let facs = dyn_list(&fac, "facilities");
    assert_eq!(facs.len(), 1);
    assert_eq!(dyn_u64(facs[0], "id.sequential"), Some(60_003_760));
    assert_eq!(dyn_u64(facs[0], "attributes.solar_system.sequential"), Some(30_000_142));
    assert_eq!(dyn_u64(facs[0], "attributes.region.sequential"), Some(10_000_002));
    assert_eq!(dyn_bool(facs[0], "attributes.online"), Some(true));

    let cost = quasar_ok(&ctx, &mut sess, "eve.industry.system.GetCostIndicesRequest", Vec::new(), b"ind-03", "GetCostIndicesResponse").await;
    let indices = dyn_list(&cost, "cost_indices");
    assert_eq!(indices.len(), 1);
    assert_eq!(dyn_u64(indices[0], "solar_system.sequential"), Some(30_000_142));

    // 2. Planet & Skyhook (+ admin gate check)
    let mut planet_payload = Vec::new();
    encode_id_field(1, 40_000_001, &mut planet_payload);
    let pln = quasar_ok(&ctx, &mut sess, "eve.planet.GetRequest", planet_payload.clone(), b"pln-01", "planet.GetResponse").await;
    assert_eq!(dyn_u64(&pln, "planet.solarsystem.sequential"), Some(30_000_142));
    assert_eq!(dyn_str(&pln, "planet.name"), Some("Planet #40000001"));
    assert_eq!(dyn_u64(&pln, "planet.type.sequential"), Some(11));

    let mut sys_payload = Vec::new();
    encode_id_field(1, 30_000_142, &mut sys_payload);
    let sys_planets = quasar_ok(&ctx, &mut sess, "eve.planet.GetPlanetsInSolarSystemRequest", sys_payload, b"pln-02", "GetPlanetsInSolarSystemResponse").await;
    let p_list = dyn_list(&sys_planets, "planets");
    assert_eq!(p_list.len(), 8);
    assert_eq!(dyn_u64(p_list[0], "sequential"), Some(3_000_014_201));

    assert_eq!(
        quasar_send(&ctx, &mut sess, "eve.planet.skyhook.admin.api.ChangeOwnerRequest", Vec::new(), b"pln-03-deny").await.status_code,
        STATUS_FORBIDDEN,
        "non-GM ChangeOwnerRequest must fail with 403"
    );
    let saved_role = sess.role;
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    quasar_ok(&ctx, &mut sess, "eve.planet.skyhook.admin.api.ChangeOwnerRequest", Vec::new(), b"pln-03", "ChangeOwnerResponse").await;
    sess.role = saved_role;

    let reinf = quasar_ok(&ctx, &mut sess, "eve.planet.skyhook.api.GetReinforcementExitHourRequest", planet_payload.clone(), b"pln-04", "GetReinforcementExitHourResponse").await;
    assert_eq!(dyn_u64(&reinf, "reinforcement_exit_hour"), Some(18));

    let sky_state = quasar_ok(&ctx, &mut sess, "eve.planet.skyhook.api.GetStateRequest", planet_payload.clone(), b"pln-05", "GetStateResponse").await;
    assert_eq!(dyn_bool(&sky_state, "no_timer"), Some(true));

    // 3. Planet Interaction (collector, factory, schematic, storage)
    let mut pi_payload = Vec::new();
    encode_id_field(1, 55001, &mut pi_payload);
    let col = quasar_ok(&ctx, &mut sess, "eve.planetinteraction.collector.GetRequest", pi_payload.clone(), b"pi-01", "collector.GetResponse").await;
    assert_eq!(dyn_u64(&col, "collector.character.sequential"), Some(90_000_001));
    assert_eq!(dyn_u64(&col, "collector.planet.sequential"), Some(55001));

    let fact = quasar_ok(&ctx, &mut sess, "eve.planetinteraction.factory.GetRequest", pi_payload.clone(), b"pi-02", "factory.GetResponse").await;
    assert_eq!(dyn_u64(&fact, "factory.character.sequential"), Some(90_000_001));
    assert_eq!(dyn_u64(&fact, "factory.planet.sequential"), Some(55001));

    let sch = quasar_ok(&ctx, &mut sess, "eve.planetinteraction.schematic.GetRequest", pi_payload.clone(), b"pi-03", "schematic.GetResponse").await;
    assert_eq!(dyn_u64(&sch, "schematic.cycle_time_sec"), Some(3600));
    assert_eq!(dyn_str(&sch, "schematic.schematic_name"), Some("PI Schematic #55001"));

    let stg = quasar_ok(&ctx, &mut sess, "eve.planetinteraction.storage.GetRequest", pi_payload, b"pi-04", "storage.GetResponse").await;
    assert_eq!(dyn_u64(&stg, "storage.character.sequential"), Some(90_000_001));
    assert_eq!(dyn_u64(&stg, "storage.planet.sequential"), Some(55001));

    // 4. Moon & Asteroid Belt
    let mut moon_payload = Vec::new();
    encode_id_field(1, 40_000_101, &mut moon_payload);
    let moon = quasar_ok(&ctx, &mut sess, "eve.moon.GetRequest", moon_payload, b"mn-01", "moon.GetResponse").await;
    assert_eq!(dyn_str(&moon, "attributes.name"), Some("Moon #40000101"));
    assert_eq!(dyn_u64(&moon, "attributes.solar_system.sequential"), Some(30_000_142));

    let moons = quasar_ok(&ctx, &mut sess, "eve.moon.GetMoonsOfPlanetRequest", planet_payload.clone(), b"mn-02", "GetMoonsOfPlanetResponse").await;
    let m_list = dyn_list(&moons, "moons");
    assert_eq!(m_list.len(), 4);
    assert_eq!(dyn_u64(m_list[0], "sequential"), Some(400_000_011));

    let mut belt_payload = Vec::new();
    encode_id_field(1, 40_000_201, &mut belt_payload);
    let belt = quasar_ok(&ctx, &mut sess, "eve.asteroidbelt.GetRequest", belt_payload, b"ab-01", "asteroidbelt.GetResponse").await;
    assert_eq!(dyn_str(&belt, "attributes.name"), Some("Asteroid Belt #40000201"));
    assert_eq!(dyn_u64(&belt, "attributes.solar_system.sequential"), Some(30_000_142));

    let belts = quasar_ok(&ctx, &mut sess, "eve.asteroidbelt.GetBeltsOfPlanetRequest", planet_payload, b"ab-02", "GetBeltsOfPlanetResponse").await;
    let b_list = dyn_list(&belts, "belts");
    assert_eq!(b_list.len(), 2);
    assert_eq!(dyn_u64(b_list[0], "sequential"), Some(4_000_000_101));
}

#[tokio::test]
async fn test_p1_track_a_regressions_issues_20_21_22() {
    let ctx = ServerContext::new();

    // 1. Issue #20: Contract Creation Fees with empty title / 14+ args & Corporate Auction FinishAuction payout
    let mut sess_corp = SessionState::new(2001);
    sess_corp.user_id = Some(1);
    sess_corp.character_id = Some(90_000_001);
    sess_corp.corporation_id = Some(98_000_001);
    sess_corp.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    sess_corp.station_id = Some(60_003_760);

    let (item_id_fee, item_id_auc) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = 98_000_001;
            ch.station_id = Some(60_003_760);
            ch.balance = eve_db::IskCents::from_isk(100_000_000);
        }
        snap.corporations.insert(
            98_000_001,
            { let mut __corp = eve_db::CorporationRecord::default();
                __corp.corporation_id = 98_000_001;
                __corp.name = "Test Corp".into();
                __corp.ticker = "TSTC".into();
                __corp.ceo_id = 90_000_001;
                __corp.set_division_balance(1, eve_db::IskCents::from_isk(50_000_000));
                __corp },
        );
        snap.next_entity_id += 1;
        let i1 = snap.next_entity_id;
        snap.items.insert(
            i1,
            eve_db::InventoryItemRecord {
                item_id: i1,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let i2 = snap.next_entity_id;
        snap.items.insert(
            i2,
            eve_db::InventoryItemRecord {
                item_id: i2,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        (i1, i2)
    });

    let bal_before = ctx.db.get_character(90_000_001).unwrap().balance.0;
    let create_rep = rpc(
        &ctx,
        &mut sess_corp,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(1), // ItemExchange
            PyRep::Integer(0), // Public
            PyRep::Integer(0),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(60_003_760),
            PyRep::Integer(60_003_760),
            PyRep::Real(1_000_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::String("".into()), // Empty title!
            PyRep::String("".into()),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(item_id_fee), PyRep::Integer(100)])]),
            PyRep::Integer(4), // 14th arg!
        ],
    )
    .await
    .expect("CreateContract with empty title and 14 args");
    let _ = create_rep;
    let bal_after = ctx.db.get_character(90_000_001).unwrap().balance.0;
    assert!(
        bal_after < bal_before,
        "Issue #20: CreateContract must charge broker fee + deposit even when title is empty and 14+ args are passed"
    );

    // Corporate auction FinishAuction routes proceeds to corporation wallet
    let corp_auc_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let cid = snap.next_entity_id;
        if let Some(it) = snap.items.get_mut(&item_id_auc) {
            it.location_id = cid;
            it.flag = eve_db::InventoryFlag::Locked;
        }
        snap.contracts.insert(
            cid,
            eve_db::ContractRecord {
                contract_id: cid,
                issuer_id: 90_000_001,
                issuer_corp_id: 98_000_001,
                contract_type: "Auction".into(),
                status: "Outstanding".into(),
                availability: "Public".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                price: eve_db::IskCents::from_isk(1_000_000),
                current_bid: eve_db::IskCents::from_isk(2_500_000),
                current_bidder_id: Some(90_000_002),
                item_ids: vec![item_id_auc],
                expiration_unix: 100,
                for_corp: true,
                ..Default::default()
            },
        );
        cid
    });
    let corp_bal_before = ctx.db.with_snapshot(|snap| snap.corporations[&98_000_001].master_wallet_balance().0);
    let char_bal_before = ctx.db.get_character(90_000_001).unwrap().balance.0;
    rpc(
        &ctx,
        &mut sess_corp,
        "contractProxy",
        "FinishAuction",
        &[PyRep::Integer(corp_auc_id)],
    )
    .await
    .expect("FinishAuction on corporate auction");
    let corp_bal_after = ctx.db.with_snapshot(|snap| snap.corporations[&98_000_001].master_wallet_balance().0);
    let char_bal_after = ctx.db.get_character(90_000_001).unwrap().balance.0;
    assert_eq!(
        corp_bal_after - corp_bal_before,
        250_000_000,
        "Issue #20: Corporate auction FinishAuction must credit the selling corporation's master wallet"
    );
    assert_eq!(
        char_bal_after, char_bal_before,
        "Issue #20: Corporate auction FinishAuction must not credit the personal character wallet"
    );

    // 2. Issue #22: Quasar Financial Data Authorization
    let mut sess_other = SessionState::new(2002);
    sess_other.user_id = Some(2);
    sess_other.character_id = Some(90_000_002);
    sess_other.corporation_id = Some(1_000_009);
    sess_other.corp_role = 0;

    let mut target_char_payload = Vec::new();
    encode_id_field(1, 90_000_001, &mut target_char_payload);
    let rsp = quasar_send(
        &ctx,
        &mut sess_other,
        "eve.accounting.character.journal.GetCharacterEntriesRequest",
        target_char_payload.clone(),
        b"sec-22-1",
    )
    .await;
    assert_eq!(rsp.status_code, 403, "Issue #22: Cross-character wallet journal query must return 403");

    let rsp = quasar_send(
        &ctx,
        &mut sess_other,
        "eve.market.orders.GetOpenForCharacterRequest",
        target_char_payload,
        b"sec-22-2",
    )
    .await;
    assert_eq!(rsp.status_code, 403, "Issue #22: Cross-character market order query must return 403");

    let mut target_corp_payload = Vec::new();
    encode_id_field(1, 98_000_001, &mut target_corp_payload);
    let rsp = quasar_send(
        &ctx,
        &mut sess_other,
        "eve.market.orders.GetOpenForCorporationRequest",
        target_corp_payload,
        b"sec-22-3",
    )
    .await;
    assert_eq!(rsp.status_code, 403, "Issue #22: Cross-corporation market order query must return 403");
}

}
