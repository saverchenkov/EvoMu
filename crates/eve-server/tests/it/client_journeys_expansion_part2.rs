//! Authentic Live EVE Client Market UI Journey Test Driver (Part 2 of `client_journeys_expansion`).

#![forbid(unsafe_code)]

use crate::client_journeys_expansion::{call_rpc, dict_get};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::sync::Arc;

#[tokio::test]
async fn test_v24_client_market_ui_every_tab_and_click_flow() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(31_099);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");

    let station_id = 60_003_760i64;
    let sys_id = 30_000_142u32;
    let const_id = 20_000_020u32;
    let region_id = 10_000_002u32;
    let char_id = 90_000_001i64;
    let seller_id = 90_000_002i64;
    let trit_type_id = 34u32;
    let pyerite_type_id = 35u32;

    // Seed seller order & buyer inventory for selling
    let sell_item_id = 889_101i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            889_001,
            MarketOrderRecord {
                order_id: 889_001,
                owner_id: seller_id,
                station_id,
                region_id,
                type_id: trit_type_id,
                is_bid: false,
                price: IskCents::from_isk(5),
                vol_entered: 2_000,
                vol_remaining: 2_000,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.items.insert(
            sell_item_id,
            InventoryItemRecord {
                item_id: sell_item_id,
                type_id: pyerite_type_id,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // 1. RegionalMarket startup & Browse/Groups (`marketbase.pyj:PopulateAsksForMyRange`, `ConvertTuplesToBestByOrders`)
    let _ = call_rpc(&ctx, &mut sess, "marketProxy", "StartupCheck", &[])
        .await
        .expect("StartupCheck");
    let groups = call_rpc(&ctx, &mut sess, "marketProxy", "GetMarketGroups", &[])
        .await
        .expect("GetMarketGroups");
    assert!(groups.row_count().unwrap_or(0) > 0, "GetMarketGroups must return non-empty rowset");

    for summary_method in ["GetStationAsks", "GetSystemAsks", "GetRegionBest"] {
        let summary_rep = call_rpc(&ctx, &mut sess, "marketProxy", summary_method, &[])
            .await
            .expect(summary_method);
        assert!(summary_rep.is_cindexed_rowset(), "{summary_method} must return CIndexedRowset");
        let entries = summary_rep.cindexed_rowset_entries().expect("cindexed entries");
        let (_, trit_row) = entries
            .iter()
            .find(|(k, _)| k.as_i64() == Some(trit_type_id as i64))
            .expect("Tritanium in market summary");
        let PyRep::DBRow { columns, .. } = trit_row else {
            panic!("Expected DBRow in {summary_method}");
        };
        assert_eq!(columns[0].0, "price", "{summary_method} col 0 must be price for BestByOrder(*order)");
        assert_eq!(columns[1].0, "volRemaining", "{summary_method} col 1 must be volRemaining for BestByOrder(*order)");
        assert_eq!(columns[2].0, "typeID", "{summary_method} col 2 must be typeID for BestByOrder(*order)");
        assert_eq!(columns[3].0, "stationID", "{summary_method} col 3 must be stationID for BestByOrder(*order)");
        assert!(columns[0].1.as_f64().unwrap_or(0.0) > 0.0);
        assert_eq!(columns[2].1.as_i64(), Some(trit_type_id as i64));
        assert_eq!(columns[3].1.as_i64(), Some(station_id));
    }

    // 2. Order Book & RefreshJumps + SortAsks/SortBids (`marketsvc.pyj:3365-3396`, `169-386`)
    let orders_rep = call_rpc(&ctx, &mut sess, "marketProxy", "GetOrders", &[PyRep::Integer(trit_type_id as i64)])
        .await
        .expect("GetOrders");
    let PyRep::Tuple(books) = orders_rep else {
        panic!("GetOrders must return (asks, bids) Tuple");
    };
    assert_eq!(books.len(), 2);
    for (idx, book) in books.iter().enumerate() {
        assert!(book.is_crowset(), "GetOrders book {idx} must be CRowset");
        for row in book.crowset_rows().unwrap() {
            for col in [
                "price", "volRemaining", "typeID", "orderID", "volEntered", "minVolume",
                "bid", "stationID", "regionID", "range", "issueDate", "duration",
                "solarSystemID", "constellationID", "jumps",
            ] {
                assert!(
                    row.dbrow_get(col).is_some(),
                    "GetOrders DBRow missing required V24.01 column {col}"
                );
            }
            assert_eq!(row.dbrow_get("solarSystemID"), Some(&PyRep::Integer(sys_id as i64)));
            assert_eq!(row.dbrow_get("constellationID"), Some(&PyRep::Integer(const_id as i64)));
            assert_eq!(row.dbrow_get("jumps"), Some(&PyRep::Integer(0)));
        }
    }

    // 3. PlaceBuyOrder, PlaceMultiSellOrder, ModifyCharOrder, CancelCharOrder & OnOwnOrdersChanged payload
    let _ = ctx.drain_notifications_for_char(char_id);
    let buy_res = call_rpc(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(station_id),
            PyRep::Integer(trit_type_id as i64),
            PyRep::Real(3.50),
            PyRep::Integer(100),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("PlaceBuyOrder");
    let buy_oid = dict_get(&buy_res, "orderID").and_then(|v| v.as_i64()).expect("buy orderID");

    let sell_item_kv = PyRep::keyval([
        ("stationID", PyRep::Integer(station_id)),
        ("typeID", PyRep::Integer(pyerite_type_id as i64)),
        ("itemID", PyRep::Integer(sell_item_id)),
        ("price", PyRep::Real(12.50)),
        ("quantity", PyRep::Integer(200)),
        ("officeID", PyRep::None),
        ("delta", PyRep::Real(0.0)),
        ("rawBrokerFeePercentage", PyRep::Real(0.03)),
    ]);
    let sell_res = call_rpc(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![sell_item_kv]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.03),
        ],
    )
    .await
    .expect("PlaceMultiSellOrder");
    let sell_oid = dict_get(&sell_res, "orderID").and_then(|v| v.as_i64()).expect("sell orderID");

    let notifs = ctx.drain_notifications_for_char(char_id);
    let own_order_notifs: Vec<_> = notifs.iter().filter(|n| n.event == "OnOwnOrdersChanged").collect();
    assert!(!own_order_notifs.is_empty(), "Must emit OnOwnOrdersChanged on order placement");
    for n in own_order_notifs {
        let Some(PyRep::List(order_list)) = n.args.first() else {
            panic!("OnOwnOrdersChanged arg 0 must be a List of order KeyVals, got {:?}", n.args.first());
        };
        assert!(!order_list.is_empty());
        for ord in order_list {
            for attr in ["orderID", "typeID", "bid", "duration", "volRemaining", "price", "stationID", "regionID", "solarSystemID"] {
                assert!(ord.dict_get(attr).is_some(), "OnOwnOrdersChanged order KeyVal missing {attr}");
            }
        }
    }

    // 4. PersonalMarketOrders (`GetCharOrders`), CorpMarketOrders (`GetCorporationOrders`), Escrow (`GetCharEscrow`)
    let char_orders = call_rpc(&ctx, &mut sess, "marketProxy", "GetCharOrders", &[])
        .await
        .expect("GetCharOrders");
    let PyRep::Rowset { columns: co_cols, rows: co_rows } = char_orders else {
        panic!("GetCharOrders must return Rowset");
    };
    for req_col in [
        "orderID", "typeID", "bid", "price", "volRemaining", "volEntered",
        "stationID", "regionID", "solarSystemID", "constellationID", "range",
        "issueDate", "duration", "minVolume", "escrow", "charID", "orderState",
        "keyID", "accountID", "isCorp",
    ] {
        assert!(co_cols.iter().any(|c| c == req_col), "GetCharOrders missing column {req_col}");
    }
    assert!(co_rows.len() >= 2, "GetCharOrders must include placed buy and sell orders");

    let corp_orders = call_rpc(&ctx, &mut sess, "marketProxy", "GetCorporationOrders", &[])
        .await
        .expect("GetCorporationOrders");
    let PyRep::Rowset { columns: corp_cols, .. } = corp_orders else {
        panic!("GetCorporationOrders must return Rowset");
    };
    for req_col in ["orderID", "typeID", "charID", "regionID", "stationID", "volEntered", "volRemaining", "keyID", "accountID", "isCorp", "solarSystemID", "constellationID"] {
        assert!(corp_cols.iter().any(|c| c == req_col), "GetCorporationOrders missing column {req_col}");
    }

    let escrow_rep = call_rpc(&ctx, &mut sess, "marketProxy", "GetCharEscrow", &[])
        .await
        .expect("GetCharEscrow");
    let isk_escrow = dict_get(&escrow_rep, "iskEscrow").and_then(|v| v.as_f64()).expect("iskEscrow");
    let items_escrow = dict_get(&escrow_rep, "itemsEscrow").and_then(|v| v.as_f64()).expect("itemsEscrow");
    assert!(isk_escrow >= 350.0, "iskEscrow must reflect open buy order escrow");
    assert!((items_escrow - 2_500.0).abs() < 1e-2, "itemsEscrow must reflect open sell order value (200 * 12.50)");

    // 5. ModifyCharOrder & CancelCharOrder (2-arg client signature: orderID, regionID) + GetMarketOrderHistory
    ctx.advance_virtual_clock(301);
    let _ = call_rpc(
        &ctx,
        &mut sess,
        "marketProxy",
        "ModifyCharOrder",
        &[
            PyRep::Integer(buy_oid),
            PyRep::Real(3.60),
            PyRep::Bool(true),
            PyRep::Integer(station_id),
            PyRep::Integer(-1),
            PyRep::Real(3.50),
        ],
    )
    .await
    .expect("ModifyCharOrder");
    let _ = call_rpc(
        &ctx,
        &mut sess,
        "marketProxy",
        "CancelCharOrder",
        &[PyRep::Integer(buy_oid), PyRep::Integer(region_id as i64)],
    )
    .await
    .expect("CancelCharOrder");
    let _ = call_rpc(
        &ctx,
        &mut sess,
        "marketProxy",
        "CancelCharOrder",
        &[PyRep::Integer(sell_oid), PyRep::Integer(region_id as i64)],
    )
    .await
    .expect("CancelCharOrder sell");

    let hist_orders = call_rpc(&ctx, &mut sess, "marketProxy", "GetMarketOrderHistory", &[])
        .await
        .expect("GetMarketOrderHistory");
    let PyRep::Rowset { columns: mh_cols, rows: mh_rows } = hist_orders else {
        panic!("GetMarketOrderHistory must return Rowset");
    };
    for req_col in [
        "orderID", "typeID", "charID", "regionID", "stationID", "range", "bid",
        "price", "volEntered", "volRemaining", "issueDate", "orderState",
        "duration", "isCorp", "solarSystemID", "lastStateChange", "keyID", "constellationID",
    ] {
        assert!(mh_cols.iter().any(|c| c == req_col), "GetMarketOrderHistory missing column {req_col}");
    }
    assert!(mh_rows.len() >= 2, "Cancelled orders must appear in GetMarketOrderHistory");

    // 6. V24.01 3-arg BuyMultipleItems (`BuyMulti(stationID, itemList, useCorp)`) + Price History (`set` arg) + Transactions
    let buy_info = PyRep::keyval([
        ("stationID", PyRep::Integer(station_id)),
        ("typeID", PyRep::Integer(trit_type_id as i64)),
        ("price", PyRep::Real(5.0)),
        ("quantity", PyRep::Integer(400)),
        ("minVolume", PyRep::Integer(1)),
        ("delta", PyRep::Real(0.0)),
    ]);
    let orders_created = call_rpc(
        &ctx,
        &mut sess,
        "marketProxy",
        "BuyMultipleItems",
        &[
            PyRep::Integer(station_id),
            PyRep::List(vec![buy_info]),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("3-arg BuyMultipleItems (BuyMulti)");
    let PyRep::List(created_list) = orders_created else {
        panic!("3-arg BuyMultipleItems must return List of order KeyVals for BuyMulti");
    };
    assert_eq!(created_list.len(), 1);
    assert_eq!(created_list[0].dict_get("typeID"), Some(&PyRep::Integer(trit_type_id as i64)));
    assert!(created_list[0].dict_get("orderID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    let set_hist = call_rpc(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetHistoryForManyTypeIDs",
        &[PyRep::PySet(vec![PyRep::Integer(trit_type_id as i64), PyRep::Integer(pyerite_type_id as i64)])],
    )
    .await
    .expect("GetHistoryForManyTypeIDs with Python set");
    let PyRep::Dict(hist_entries) = set_hist else {
        panic!("GetHistoryForManyTypeIDs must return Dict");
    };
    assert_eq!(hist_entries.len(), 2);
    for (_, val) in &hist_entries {
        let PyRep::Tuple(pair) = val else {
            panic!("GetHistoryForManyTypeIDs(set) value must be (old, new) 2-tuple");
        };
        assert_eq!(pair.len(), 2);
        assert!(pair[0].is_crowset() && pair[1].is_crowset());
    }

    let char_tx = call_rpc(&ctx, &mut sess, "marketProxy", "CharGetTransactions", &[PyRep::Integer(0)])
        .await
        .expect("CharGetTransactions");
    let PyRep::Rowset { columns: tx_cols, rows: tx_rows } = char_tx else {
        panic!("CharGetTransactions must return Rowset");
    };
    for req_col in [
        "transactionID", "transactionDate", "typeID", "quantity", "price",
        "stationID", "buyerID", "sellerID", "accountID", "buyerAccountID", "sellerAccountID",
    ] {
        assert!(tx_cols.iter().any(|c| c == req_col), "CharGetTransactions missing column {req_col}");
    }
    assert!(!tx_rows.is_empty(), "CharGetTransactions must contain executed BuyMulti trade");

    let corp_tx = call_rpc(&ctx, &mut sess, "marketProxy", "CorpGetTransactions", &[PyRep::Integer(0), PyRep::Integer(1000)])
        .await
        .expect("CorpGetTransactions");
    assert!(matches!(corp_tx, PyRep::Rowset { .. }));

    let full_types = eve_server::world_seeder::select_full_market_types(&ctx.sde);
    assert!(
        full_types.len() >= 11_000,
        "Full market seeder catalog must include >= 11,000 published gameplay types, got {}",
        full_types.len()
    );
    let added = eve_server::world_seeder::seed_full_market_catalog(&ctx.db, &ctx.sde);
    assert!(added > 10_000, "seed_full_market_catalog must insert full Jita order book, got {added}");

    let dc2_orders = call_rpc(&ctx, &mut sess, "marketProxy", "GetOrders", &[PyRep::Integer(2048)])
        .await
        .expect("GetOrders for Damage Control II (2048)");
    let PyRep::Tuple(dc2_pair) = dc2_orders else {
        panic!("GetOrders(2048) must return 2-tuple");
    };
    let dc2_asks = dc2_pair[0].crowset_rows().expect("DC2 asks CRowset");
    let dc2_bids = dc2_pair[1].crowset_rows().expect("DC2 bids CRowset");
    assert!(!dc2_asks.is_empty(), "Damage Control II must have seeded Jita sell orders");
    assert!(!dc2_bids.is_empty(), "Damage Control II must have seeded Jita buy orders");
    assert!(
        dc2_asks[0].dbrow_get("price").and_then(|v| v.as_f64()).unwrap_or(0.0) > 100.0,
        "Damage Control II ask price must be derived from BOM (> 100 ISK)"
    );
}
