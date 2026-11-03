//! `ESI-6` integration tests (part 5) for character and corporation market orders.

#![forbid(unsafe_code)]

use super::esi_market_industry_pi::{
    assert_status, get_json, issue_token_with_scopes, make_corp, make_item, rpc_call,
};
use eve_db::{
    corp_roles::{ROLE_ACCOUNTANT, ROLE_DIRECTOR, ROLE_TRADER},
    InventoryFlag, IskCents, TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn test_esi6_market_character_and_corporation_orders_routes() {
    // parity: esi:GET /characters/{character_id}/orders, esi:GET /characters/{character_id}/orders/history
    // parity: esi:GET /corporations/{corporation_id}/orders, esi:GET /corporations/{corporation_id}/orders/history
    // parity: feat:esi.market.char-orders, feat:esi.market.corp-orders
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/orders",
        "esi:GET /characters/{character_id}/orders/history",
        "esi:GET /corporations/{corporation_id}/orders",
        "esi:GET /corporations/{corporation_id}/orders/history",
        "feat:esi.market.char-orders",
        "feat:esi.market.corp-orders",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_611_i64;
    let jita_station = 60_003_760_i64;
    let isogen_item_id = 920_010_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(jita_station);
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_isk(250_000_000);
        }
        snap.corporations
            .insert(corp_id, make_corp(corp_id, "ESI6 Order Corp", "E6OC", char_id, None, 0.0));
        snap.skills.entry(char_id).or_default().insert(
            3447,
            TrainedSkillRecord {
                type_id: 3447,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.items.insert(
            isogen_item_id,
            make_item(isogen_item_id, 37, corp_id, jita_station, InventoryFlag::Hangar, 2_500, false),
        );
    });

    let mut sess = SessionState::new(9810);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.corp_role = ROLE_DIRECTOR | ROLE_ACCOUNTANT | ROLE_TRADER;
    sess.station_id = Some(jita_station);
    sess.solar_system_id = Some(30_000_142);
    sess.region_id = Some(10_000_002);

    let char_active_args = [
        PyRep::Integer(jita_station),
        PyRep::Integer(34),
        PyRep::Real(3.5),
        PyRep::Integer(2_000),
        PyRep::Integer(0),
        PyRep::Integer(5),
        PyRep::Integer(7),
        PyRep::Bool(false),
    ];
    let char_active_rep =
        rpc_call(&ctx, &mut sess, "marketProxy", "PlaceBuyOrder", &char_active_args).await;
    let char_active_oid = char_active_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();

    let char_hist_args = [
        PyRep::Integer(jita_station),
        PyRep::Integer(35),
        PyRep::Real(4.0),
        PyRep::Integer(4_000),
        PyRep::Integer(-1),
        PyRep::Integer(1),
        PyRep::Integer(14),
        PyRep::Bool(false),
    ];
    let char_hist_rep =
        rpc_call(&ctx, &mut sess, "marketProxy", "PlaceBuyOrder", &char_hist_args).await;
    let char_hist_oid = char_hist_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    rpc_call(&ctx, &mut sess, "marketProxy", "CancelCharOrder", &[PyRep::Integer(char_hist_oid)])
        .await;

    let corp_active_args = [
        PyRep::Integer(jita_station),
        PyRep::Integer(36),
        PyRep::Real(25.0),
        PyRep::Integer(800),
        PyRep::Integer(-1),
        PyRep::Integer(10),
        PyRep::Integer(30),
        PyRep::Bool(true),
        PyRep::Integer(1001),
    ];
    let corp_active_rep =
        rpc_call(&ctx, &mut sess, "marketProxy", "PlaceBuyOrder", &corp_active_args).await;
    let corp_active_oid = corp_active_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();

    let corp_sell_items = PyRep::List(vec![PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(isogen_item_id)),
        (PyRep::String("typeID".into()), PyRep::Integer(37)),
        (PyRep::String("price".into()), PyRep::Real(95.0)),
        (PyRep::String("quantity".into()), PyRep::Integer(2_500)),
        (PyRep::String("stationID".into()), PyRep::Integer(jita_station)),
        (PyRep::String("duration".into()), PyRep::Integer(90)),
    ])]);
    let corp_hist_args = [
        corp_sell_items,
        PyRep::Bool(true),
        PyRep::Integer(90),
        PyRep::None,
        PyRep::Integer(1002),
    ];
    let corp_hist_rep =
        rpc_call(&ctx, &mut sess, "marketProxy", "PlaceMultiSellOrder", &corp_hist_args).await;
    let corp_hist_oid = corp_hist_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    rpc_call(&ctx, &mut sess, "marketProxy", "CancelCharOrder", &[PyRep::Integer(corp_hist_oid)])
        .await;

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-markets.read_character_orders.v1",
            "esi-markets.read_corporation_orders.v1",
        ],
    );
    let no_scope_token = issue_token_with_scopes(&ctx, char_id, vec![]);

    assert_status(&router, format!("/v2/characters/{char_id}/orders/"), None, 401);
    assert_status(&router, format!("/v2/characters/{char_id}/orders/"), Some(&no_scope_token), 403);

    let char_orders =
        get_json(&router, &spec, format!("/v2/characters/{char_id}/orders/"), Some(&token));
    let char_orders_arr = char_orders.as_array().unwrap();
    assert_eq!(char_orders_arr.len(), 1);
    assert_eq!(char_orders_arr[0]["order_id"], json!(char_active_oid));
    assert_eq!(char_orders_arr[0]["volume_remain"], json!(2_000));
    assert_eq!(char_orders_arr[0]["min_volume"], json!(5));
    assert_eq!(char_orders_arr[0]["duration"], json!(7));
    assert_eq!(char_orders_arr[0]["range"], json!("solarsystem"));
    assert_eq!(char_orders_arr[0]["is_corporation"], json!(false));

    let char_hist =
        get_json(&router, &spec, format!("/v1/characters/{char_id}/orders/history/"), Some(&token));
    let char_hist_arr = char_hist.as_array().unwrap();
    assert_eq!(char_hist_arr.len(), 1);
    assert_eq!(char_hist_arr[0]["order_id"], json!(char_hist_oid));
    assert_eq!(char_hist_arr[0]["state"], json!("cancelled"));
    assert_eq!(char_hist_arr[0]["duration"], json!(14));
    assert_status(&router, format!("/v1/characters/{char_id}/orders/history/?page=2"), Some(&token), 404);

    let corp_orders =
        get_json(&router, &spec, format!("/v3/corporations/{corp_id}/orders/"), Some(&token));
    let corp_orders_arr = corp_orders.as_array().unwrap();
    assert_eq!(corp_orders_arr.len(), 1);
    assert_eq!(corp_orders_arr[0]["order_id"], json!(corp_active_oid));
    assert_eq!(corp_orders_arr[0]["wallet_division"], json!(2));
    assert_eq!(corp_orders_arr[0]["issued_by"], json!(char_id));
    assert_eq!(corp_orders_arr[0]["duration"], json!(30));
    assert_eq!(corp_orders_arr[0]["range"], json!("station"));

    let corp_hist = get_json(
        &router,
        &spec,
        format!("/v2/corporations/{corp_id}/orders/history/"),
        Some(&token),
    );
    let corp_hist_arr = corp_hist.as_array().unwrap();
    assert_eq!(corp_hist_arr.len(), 1);
    assert_eq!(corp_hist_arr[0]["order_id"], json!(corp_hist_oid));
    assert_eq!(corp_hist_arr[0]["wallet_division"], json!(3));
    assert_eq!(corp_hist_arr[0]["state"], json!("cancelled"));
    assert_eq!(corp_hist_arr[0]["duration"], json!(90));
}
