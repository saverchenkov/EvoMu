//! Integration tests for Market Price History remediation:
//! Verifies that `marketProxy.GetOldPriceHistory`, `GetNewPriceHistory`, and
//! `GetHistoryForManyTypeIDs` query authentic trade aggregates from executed
//! market orders rather than fabricating static synthetic numbers, and that
//! un-traded types return an empty rowset.

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TypeId};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_REGION_ID: u32 = 10_000_002;
const JITA_SYSTEM_ID: u32 = 30_000_142;
const JITA_STATION_ID: i64 = 60_003_760;
const TRITANIUM: TypeId = 34;
const UNTRADED_TYPE: TypeId = 99_999;

const SELLER_CHAR_ID: i64 = 90_000_001;
const BUYER_CHAR_ID: i64 = 90_000_002;

async fn setup_pilot(ctx: &ServerContext, session_id: u64, username: &str, password: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    sess.station_id = Some(JITA_STATION_ID);
    sess.solar_system_id = Some(JITA_SYSTEM_ID);
    sess.region_id = Some(JITA_REGION_ID);
    sess
}

fn seed_hangar_item(snap: &mut eve_db::WorldStateSnapshot, type_id: TypeId, owner_id: i64, location_id: i64, quantity: i32) -> i64 {
    snap.next_entity_id += 1;
    let item_id = snap.next_entity_id;
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag: InventoryFlag::Hangar,
            quantity,
            singleton: false,
            version: 1,
            ..Default::default()
        },
    );
    item_id
}

/// Comprehensive test for market price history remediation:
/// Verifies authentic OHLC, volume, and order-count calculation on executed trades,
/// and empty rowsets for un-traded types.
#[tokio::test]
async fn test_market_price_history_authentic_aggregates_and_empty_untraded() {
    // parity: feat:market.price-history, rpc:marketProxy.GetHistoryForManyTypeIDs, rpc:marketProxy.GetNewPriceHistory, rpc:marketProxy.GetOldPriceHistory
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut seller_sess = setup_pilot(&ctx, 1201, "demo", "demo-secret", SELLER_CHAR_ID).await;
    let mut buyer_sess = setup_pilot(&ctx, 1202, "other", "other-secret", BUYER_CHAR_ID).await;

    // Seed seller with 50,000 Tritanium and buyer with 1,000,000 ISK; clear seeded NPC sell order
    let seller_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.remove(&700_000_001);
        if let Some(ch) = snap.characters.get_mut(&SELLER_CHAR_ID) {
            ch.balance = IskCents::from_isk(100_000);
            ch.station_id = Some(JITA_STATION_ID);
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
        if let Some(ch) = snap.characters.get_mut(&BUYER_CHAR_ID) {
            ch.balance = IskCents::from_isk(1_000_000);
            ch.station_id = Some(JITA_STATION_ID);
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
        seed_hangar_item(snap, TRITANIUM, SELLER_CHAR_ID, JITA_STATION_ID, 50_000)
    });

    let empty_kw = HashMap::new();

    // 1. Initial State: Un-traded types must return an empty CRowset (0 rows), NOT fabricated rows
    let old_hist_before = ctx
        .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetOldPriceHistory", &[PyRep::Integer(TRITANIUM as i64)], &empty_kw, None)
        .await
        .expect("GetOldPriceHistory before trades");
    assert!(old_hist_before.is_crowset(), "GetOldPriceHistory must return valid CRowset");
    let rows_before = old_hist_before.crowset_rows().expect("crowset rows");
    assert_eq!(rows_before.len(), 0, "Un-traded item must return empty price history rowset");

    let new_hist_before = ctx
        .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetNewPriceHistory", &[PyRep::Integer(TRITANIUM as i64)], &empty_kw, None)
        .await
        .expect("GetNewPriceHistory before trades");
    assert!(new_hist_before.is_crowset(), "GetNewPriceHistory must return valid CRowset");
    let rows_new_before = new_hist_before.crowset_rows().expect("crowset rows");
    assert_eq!(rows_new_before.len(), 0, "Un-traded item must return empty price history rowset");

    let many_hist_before = ctx
        .dispatch_rpc(
            &mut buyer_sess,
            "marketProxy",
            None,
            "GetHistoryForManyTypeIDs",
            &[PyRep::List(vec![PyRep::Integer(TRITANIUM as i64), PyRep::Integer(UNTRADED_TYPE as i64)])],
            &empty_kw,
            None,
        )
        .await
        .expect("GetHistoryForManyTypeIDs before trades");
    let PyRep::Dict(entries_before) = many_hist_before else {
        panic!("Expected Dict from GetHistoryForManyTypeIDs");
    };
    assert_eq!(entries_before.len(), 2);
    for (_, crowset) in &entries_before {
        assert!(crowset.is_crowset());
        assert_eq!(crowset.crowset_rows().map(|r| r.len()), Some(0));
    }

    // 2. Execute Trade 1: Seller places sell for 400 units @ 4.00 ISK; Buyer buys 400 units @ 4.00 ISK
    let sell_order_arg = PyRep::List(vec![PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(seller_item_id)),
        (PyRep::String("typeID".into()), PyRep::Integer(TRITANIUM as i64)),
        (PyRep::String("price".into()), PyRep::Real(4.00)),
        (PyRep::String("quantity".into()), PyRep::Integer(400)),
        (PyRep::String("stationID".into()), PyRep::Integer(JITA_STATION_ID)),
        (PyRep::String("duration".into()), PyRep::Integer(90)),
    ])]);
    ctx.dispatch_rpc(&mut seller_sess, "marketProxy", None, "PlaceMultiSellOrder", &[sell_order_arg, PyRep::Bool(false)], &empty_kw, None)
        .await
        .expect("PlaceMultiSellOrder 1");

    ctx.dispatch_rpc(
        &mut buyer_sess,
        "marketProxy",
        None,
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM as i64),
            PyRep::Real(4.00),
            PyRep::Integer(400),
            PyRep::Integer(32767),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
        &empty_kw,
        None,
    )
    .await
    .expect("PlaceBuyOrder 1");

    // 3. Execute Trade 2 on the same day: Seller places sell for 500 units @ 6.00 ISK; Buyer buys 200 units @ 6.00 ISK
    let seller_item_id_2 = ctx.db.with_mut_snapshot(|snap| {
        seed_hangar_item(snap, TRITANIUM, SELLER_CHAR_ID, JITA_STATION_ID, 500)
    });
    let sell_order_arg_2 = PyRep::List(vec![PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(seller_item_id_2)),
        (PyRep::String("typeID".into()), PyRep::Integer(TRITANIUM as i64)),
        (PyRep::String("price".into()), PyRep::Real(6.00)),
        (PyRep::String("quantity".into()), PyRep::Integer(500)),
        (PyRep::String("stationID".into()), PyRep::Integer(JITA_STATION_ID)),
        (PyRep::String("duration".into()), PyRep::Integer(90)),
    ])]);
    ctx.dispatch_rpc(&mut seller_sess, "marketProxy", None, "PlaceMultiSellOrder", &[sell_order_arg_2, PyRep::Bool(false)], &empty_kw, None)
        .await
        .expect("PlaceMultiSellOrder 2");

    ctx.dispatch_rpc(
        &mut buyer_sess,
        "marketProxy",
        None,
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM as i64),
            PyRep::Real(6.00),
            PyRep::Integer(200),
            PyRep::Integer(32767),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
        &empty_kw,
        None,
    )
    .await
    .expect("PlaceBuyOrder 2");

    // 4. Verify persisted snapshot daily summary
    ctx.db.with_snapshot(|snap| {
        let hist = snap.market_history.get(&(JITA_REGION_ID, TRITANIUM)).expect("market_history entry for Tritanium");
        assert_eq!(hist.len(), 1, "Expected exactly 1 daily record for today");
        let day = &hist[0];
        assert_eq!(day.volume, 600, "Volume must equal sum of fills (400 + 200)");
        assert_eq!(day.order_count, 2, "Order count must equal number of executed trades");
        assert!((day.lowest - 4.00).abs() < 1e-4, "Lowest price must be 4.00");
        assert!((day.highest - 6.00).abs() < 1e-4, "Highest price must be 6.00");
        let expected_avg = (400.0 * 4.00 + 200.0 * 6.00) / 600.0; // 2800 / 600 = 4.66666...
        assert!((day.average - expected_avg).abs() < 1e-4, "Average price must be weighted average (4.6667)");
        assert!((day.open_isk() - 4.00).abs() < 1e-4, "Open price must be first trade price (4.00)");
        assert!((day.close_isk() - 6.00).abs() < 1e-4, "Close price must be latest trade price (6.00)");
    });

    // 5. Verify marketProxy.GetOldPriceHistory returns authentic daily OHLC aggregate
    let old_hist_after = ctx
        .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetOldPriceHistory", &[PyRep::Integer(TRITANIUM as i64)], &empty_kw, None)
        .await
        .expect("GetOldPriceHistory after trades");
    assert!(old_hist_after.is_crowset());
    let rows_after = old_hist_after.crowset_rows().expect("crowset rows");
    assert_eq!(rows_after.len(), 1, "Expected exactly 1 daily aggregate row");
    let row = &rows_after[0];
    let low_price = row.dbrow_get("lowPrice").and_then(|v| v.as_f64()).expect("lowPrice");
    let high_price = row.dbrow_get("highPrice").and_then(|v| v.as_f64()).expect("highPrice");
    let avg_price = row.dbrow_get("avgPrice").and_then(|v| v.as_f64()).expect("avgPrice");
    let vol = row.dbrow_get("volume").and_then(|v| v.as_i64()).expect("volume");
    let orders = row.dbrow_get("orders").and_then(|v| v.as_i64()).expect("orders");

    assert!((low_price - 4.00).abs() < 1e-4, "lowPrice must match executed low");
    assert!((high_price - 6.00).abs() < 1e-4, "highPrice must match executed high");
    let expected_avg = 2800.0 / 600.0;
    assert!((avg_price - expected_avg).abs() < 1e-4, "avgPrice must match volume-weighted average");
    assert_eq!(vol, 600, "volume must match executed units");
    assert_eq!(orders, 2, "orders must match executed order count");

    // 6. Verify marketProxy.GetNewPriceHistory returns identical authentic aggregate
    let new_hist_after = ctx
        .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetNewPriceHistory", &[PyRep::Integer(TRITANIUM as i64)], &empty_kw, None)
        .await
        .expect("GetNewPriceHistory after trades");
    assert!(new_hist_after.is_crowset());
    let new_rows = new_hist_after.crowset_rows().expect("crowset rows");
    assert_eq!(new_rows.len(), 1);
    let new_row = &new_rows[0];
    assert!((new_row.dbrow_get("lowPrice").unwrap().as_f64().unwrap() - 4.00).abs() < 1e-4);
    assert!((new_row.dbrow_get("highPrice").unwrap().as_f64().unwrap() - 6.00).abs() < 1e-4);
    assert!((new_row.dbrow_get("avgPrice").unwrap().as_f64().unwrap() - expected_avg).abs() < 1e-4);
    assert_eq!(new_row.dbrow_get("volume").unwrap().as_i64().unwrap(), 600);
    assert_eq!(new_row.dbrow_get("orders").unwrap().as_i64().unwrap(), 2);

    // 7. Verify marketProxy.GetHistoryForManyTypeIDs
    let many_hist_after = ctx
        .dispatch_rpc(
            &mut buyer_sess,
            "marketProxy",
            None,
            "GetHistoryForManyTypeIDs",
            &[PyRep::List(vec![PyRep::Integer(TRITANIUM as i64), PyRep::Integer(UNTRADED_TYPE as i64)])],
            &empty_kw,
            None,
        )
        .await
        .expect("GetHistoryForManyTypeIDs after trades");
    let PyRep::Dict(entries_after) = many_hist_after else {
        panic!("Expected Dict from GetHistoryForManyTypeIDs");
    };
    assert_eq!(entries_after.len(), 2);

    let trit_crowset = entries_after
        .iter()
        .find(|(k, _)| k.as_i64() == Some(TRITANIUM as i64))
        .map(|(_, v)| v)
        .expect("Tritanium entry in GetHistoryForManyTypeIDs");
    let trit_rows = trit_crowset.crowset_rows().expect("trit rows");
    assert_eq!(trit_rows.len(), 1);
    let trit_row = &trit_rows[0];
    assert!((trit_row.dbrow_get("lowPrice").unwrap().as_f64().unwrap() - 4.00).abs() < 1e-4);
    assert!((trit_row.dbrow_get("highPrice").unwrap().as_f64().unwrap() - 6.00).abs() < 1e-4);
    assert_eq!(trit_row.dbrow_get("volume").unwrap().as_i64().unwrap(), 600);
    assert_eq!(trit_row.dbrow_get("orders").unwrap().as_i64().unwrap(), 2);

    let untraded_crowset = entries_after
        .iter()
        .find(|(k, _)| k.as_i64() == Some(UNTRADED_TYPE as i64))
        .map(|(_, v)| v)
        .expect("Untraded entry in GetHistoryForManyTypeIDs");
    let untraded_rows = untraded_crowset.crowset_rows().expect("untraded rows");
    assert_eq!(untraded_rows.len(), 0, "Untraded type must return empty rowset");
}

const PYERITE: TypeId = 35;

#[tokio::test]
async fn test_market_price_history_buymultiple_and_modifyorder() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut seller_sess = setup_pilot(&ctx, 1301, "demo", "demo-secret", SELLER_CHAR_ID).await;
    let mut buyer_sess = setup_pilot(&ctx, 1302, "other", "other-secret", BUYER_CHAR_ID).await;

    // Seed seller with Pyerite and buyer with ISK; clear any existing Pyerite market orders
    let seller_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.type_id != PYERITE);
        if let Some(ch) = snap.characters.get_mut(&SELLER_CHAR_ID) {
            ch.balance = IskCents::from_isk(100_000);
            ch.station_id = Some(JITA_STATION_ID);
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
        if let Some(ch) = snap.characters.get_mut(&BUYER_CHAR_ID) {
            ch.balance = IskCents::from_isk(1_000_000);
            ch.station_id = Some(JITA_STATION_ID);
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
        seed_hangar_item(snap, PYERITE, SELLER_CHAR_ID, JITA_STATION_ID, 10_000)
    });

    let empty_kw = HashMap::new();

    // 1. Seller places a resting sell order: 1,000 Pyerite @ 10.00 ISK
    let sell_order_arg = PyRep::List(vec![PyRep::keyval([
        ("itemID", PyRep::Integer(seller_item_id)),
        ("typeID", PyRep::Integer(PYERITE as i64)),
        ("price", PyRep::Real(10.00)),
        ("quantity", PyRep::Integer(1_000)),
        ("stationID", PyRep::Integer(JITA_STATION_ID)),
        ("duration", PyRep::Integer(90)),
    ])]);
    ctx.dispatch_rpc(
        &mut seller_sess,
        "marketProxy",
        None,
        "PlaceMultiSellOrder",
        &[sell_order_arg, PyRep::Bool(false)],
        &empty_kw,
        None,
    )
    .await
    .expect("PlaceMultiSellOrder for Pyerite");

    // 2. Buyer executes BuyMultipleItems for 300 units @ max 12.00 ISK
    let buy_items = PyRep::List(vec![PyRep::keyval([
        ("typeID", PyRep::Integer(PYERITE as i64)),
        ("quantity", PyRep::Integer(300)),
        ("maxPrice", PyRep::Real(12.00)),
        ("stationID", PyRep::Integer(JITA_STATION_ID)),
        ("orderRange", PyRep::Integer(-1)),
    ])]);
    let buy_mult_res = ctx
        .dispatch_rpc(
            &mut buyer_sess,
            "marketProxy",
            None,
            "BuyMultipleItems",
            &[buy_items, PyRep::Bool(false)],
            &empty_kw,
            None,
        )
        .await
        .expect("BuyMultipleItems for Pyerite");
    let PyRep::KeyVal(bm_kv) = buy_mult_res else { panic!("Expected KeyVal"); };
    assert_eq!(bm_kv.get("purchasedCount").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(bm_kv.get("totalQuantity").and_then(|v| v.as_i64()), Some(300));

    // 3. Verify price history and market transactions after BuyMultipleItems
    let old_hist_bm = ctx
        .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetOldPriceHistory", &[PyRep::Integer(PYERITE as i64)], &empty_kw, None)
        .await
        .expect("GetOldPriceHistory after BuyMultipleItems");
    let rows_bm = old_hist_bm.crowset_rows().expect("crowset rows");
    assert_eq!(rows_bm.len(), 1, "Expected 1 row after BuyMultipleItems");
    assert_eq!(rows_bm[0].dbrow_get("volume").unwrap().as_i64().unwrap(), 300);
    assert_eq!(rows_bm[0].dbrow_get("orders").unwrap().as_i64().unwrap(), 1);
    assert!((rows_bm[0].dbrow_get("avgPrice").unwrap().as_f64().unwrap() - 10.00).abs() < 1e-4);

    ctx.db.with_snapshot(|snap| {
        let has_bm_tx = snap.market_transactions.iter().any(|t| t.type_id == PYERITE && t.quantity == 300 && t.price.0 == 1_000);
        assert!(has_bm_tx, "Market transactions must record BuyMultipleItems fill");
    });

    // 4. Buyer places a non-matching buy order for 500 Pyerite @ 8.00 ISK (below 10.00 ISK ask)
    let buy_order_res = ctx
        .dispatch_rpc(
            &mut buyer_sess,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(PYERITE as i64),
                PyRep::Real(8.00),
                PyRep::Integer(500),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(90),
                PyRep::Bool(false),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("PlaceBuyOrder for Pyerite @ 8.00 ISK");
    let PyRep::KeyVal(bo_kv) = buy_order_res else { panic!("Expected KeyVal"); };
    let buy_oid = bo_kv.get("orderID").and_then(|v| v.as_i64()).expect("orderID");

    // Backdate order issue_unix by 301s to bypass modification cooldown
    let now_unix = ctx.now_unix();
    let meta_nid = -(4_000_000_000_000_i64.saturating_add(buy_oid.abs()));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(note) = snap.owner_notes.get_mut(&meta_nid) {
            note.note = format!("{{\"range\":-1,\"duration\":90,\"issue_unix\":{}}}", now_unix.saturating_sub(400));
        }
    });

    // 5. Buyer modifies the order to 10.00 ISK, crossing the resting ask of 10.00 ISK
    let mod_res = ctx
        .dispatch_rpc(
            &mut buyer_sess,
            "marketProxy",
            None,
            "ModifyCharOrder",
            &[PyRep::Integer(buy_oid), PyRep::Real(10.00)],
            &empty_kw,
            None,
        )
        .await
        .expect("ModifyCharOrder to 10.00 ISK");
    let PyRep::KeyVal(mo_kv) = mod_res else { panic!("Expected KeyVal"); };
    assert_eq!(mo_kv.get("orderID").and_then(|v| v.as_i64()), Some(buy_oid));

    // 6. Verify combined price history in GetOldPriceHistory and GetNewPriceHistory:
    // Volume was 300, now + 500 = 800; Orders was 1, now + 1 = 2; Average = 10.00 ISK
    let old_hist_mod = ctx
        .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetOldPriceHistory", &[PyRep::Integer(PYERITE as i64)], &empty_kw, None)
        .await
        .expect("GetOldPriceHistory after ModifyCharOrder");
    let rows_mod = old_hist_mod.crowset_rows().expect("crowset rows");
    assert_eq!(rows_mod.len(), 1, "Expected 1 aggregate row");
    assert_eq!(rows_mod[0].dbrow_get("volume").unwrap().as_i64().unwrap(), 800, "Volume must reflect BuyMultipleItems + ModifyCharOrder fills");
    assert_eq!(rows_mod[0].dbrow_get("orders").unwrap().as_i64().unwrap(), 2, "Orders count must be 2");
    assert!((rows_mod[0].dbrow_get("avgPrice").unwrap().as_f64().unwrap() - 10.00).abs() < 1e-4);

    let new_hist_mod = ctx
        .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetNewPriceHistory", &[PyRep::Integer(PYERITE as i64)], &empty_kw, None)
        .await
        .expect("GetNewPriceHistory after ModifyCharOrder");
    let new_rows_mod = new_hist_mod.crowset_rows().expect("crowset rows");
    assert_eq!(new_rows_mod.len(), 1);
    assert_eq!(new_rows_mod[0].dbrow_get("volume").unwrap().as_i64().unwrap(), 800);
    assert_eq!(new_rows_mod[0].dbrow_get("orders").unwrap().as_i64().unwrap(), 2);

    ctx.db.with_snapshot(|snap| {
        let has_mod_tx = snap.market_transactions.iter().any(|t| t.type_id == PYERITE && t.quantity == 500 && t.price.0 == 1_000);
        assert!(has_mod_tx, "Market transactions must record ModifyCharOrder crossed fill");
    });
}
