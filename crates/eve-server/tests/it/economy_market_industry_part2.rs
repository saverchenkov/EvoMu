//! Market Broker Fees, Sales Tax, Effective Standing, Limit Orders, Escrow, Order Book Queries,
//! and Order Cancellation integration tests (`E-1`, `E-2`).



use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_STATION_ID: i64 = 60_003_760;
const TRITANIUM_TYPE_ID: TypeId = 34;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

#[tokio::test]
async fn test_market_orders_limit_escrow_queries_cancellation() {
    // parity: feat:market.escrow, feat:market.get-orders, feat:market.place-buy, feat:market.place-sell, rpc:marketProxy.CancelCharOrder, rpc:marketProxy.GetCharEscrow, rpc:marketProxy.GetOrders, rpc:marketProxy.PlaceBuyOrder, rpc:marketProxy.PlaceMultiSellOrder
    eve_testkit::attest(&["feat:market.escrow", "feat:market.get-orders", "feat:market.place-buy", "feat:market.place-sell"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut s1 = setup_pilot(&ctx, 201, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 202, "other", "other-secret", 90_000_002).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(
            3447,
            TrainedSkillRecord {
                type_id: 3447,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });

    // Verify PlaceBuyOrder stores station (-1), solar system (0), N jumps (5), and region (32767) ranges
    for &rng in &[-1_i64, 0, 32767] {
        let r_ord = ctx.dispatch_rpc(
            &mut s1, "marketProxy", None, "PlaceBuyOrder",
            &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Real(0.20), PyRep::Integer(100), PyRep::Integer(rng), PyRep::Integer(10), PyRep::Integer(14), PyRep::Bool(false)],
            &kw, None,
        ).await.expect("PlaceBuyOrder range check");
        assert_eq!(r_ord.dict_get("range").and_then(|v| v.as_i64()), Some(rng));
        assert_eq!(r_ord.dict_get("duration").and_then(|v| v.as_i64()), Some(14));
        assert_eq!(r_ord.dict_get("minVolume").and_then(|v| v.as_i64()), Some(10));
        let roid = r_ord.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
        ctx.dispatch_rpc(&mut s1, "marketProxy", None, "CancelCharOrder", &[PyRep::Integer(roid)], &kw, None).await.unwrap();
    }

    let s1_bal_initial = ctx.db.get_character(90_000_001).unwrap().balance;

    let buy_price = 0.50;
    let buy_qty = 5_000;
    let buy_order = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(buy_price),
                PyRep::Integer(buy_qty),
                PyRep::Integer(5),
                PyRep::Integer(100),
                PyRep::Integer(30),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("PlaceBuyOrder buy");

    let PyRep::KeyVal(buy_kv) = buy_order else {
        panic!("Expected KeyVal");
    };
    assert_eq!(buy_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));
    let buy_oid = buy_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();
    let broker_fee_isk = buy_kv.get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap();

    let s1_bal_after_buy = ctx.db.get_character(90_000_001).unwrap().balance;
    let expected_escrow_cents = (buy_price * (buy_qty as f64) * 100.0) as i64;
    let expected_broker_cents = (broker_fee_isk * 100.0) as i64;
    assert_eq!(
        s1_bal_initial.0 - s1_bal_after_buy.0,
        expected_escrow_cents + expected_broker_cents
    );

    let order_rec = ctx
        .db
        .with_snapshot(|snap| snap.market_orders.get(&buy_oid).cloned())
        .unwrap();
    assert_eq!(order_rec.escrow.0, expected_escrow_cents);
    assert_eq!(order_rec.vol_remaining, buy_qty as i32);
    assert_eq!(order_rec.min_volume, 100);

    let escrow_rep = ctx
        .dispatch_rpc(&mut s1, "marketProxy", None, "GetCharEscrow", &[], &kw, None)
        .await
        .expect("GetCharEscrow");
    let PyRep::KeyVal(escrow_kv) = escrow_rep else {
        panic!("Expected KeyVal from GetCharEscrow");
    };
    assert_eq!(escrow_kv.get("totalEscrowISK"), Some(&PyRep::Real(2_500.0)));

    let orders_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "GetOrders",
            &[PyRep::Integer(TRITANIUM_TYPE_ID as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetOrders");

    let PyRep::Tuple(ref books) = orders_rep else {
        panic!("Expected Tuple from GetOrders");
    };
    assert_eq!(books.len(), 2);
    let PyRep::ObjectEx { list: ref bid_rows, .. } = books[1] else {
        panic!("Expected CRowset ObjectEx for buy orders");
    };
    let buy_row_cols = bid_rows
        .iter()
        .find_map(|r| match r {
            PyRep::DBRow { columns, .. }
                if columns.get(3).and_then(|(_, c)| c.as_i64()) == Some(buy_oid) =>
            {
                Some(columns)
            }
            _ => None,
        })
        .expect("Placed buy order must appear in GetOrders buy rowset");
    assert_eq!(buy_row_cols.get(0).map(|(_, v)| v), Some(&PyRep::Real(0.50)));
    assert_eq!(buy_row_cols.get(1).map(|(_, v)| v), Some(&PyRep::Real(5_000.0)));
    assert_eq!(
        buy_row_cols.get(2).map(|(_, v)| v),
        Some(&PyRep::Integer(TRITANIUM_TYPE_ID as i64))
    );
    assert_eq!(buy_row_cols.get(4).map(|(_, v)| v), Some(&PyRep::Integer(5_000)));
    assert_eq!(buy_row_cols.get(5).map(|(_, v)| v), Some(&PyRep::Integer(100)));
    assert_eq!(buy_row_cols.get(6).map(|(_, v)| v), Some(&PyRep::Bool(true)));
    assert_eq!(
        buy_row_cols.get(7).map(|(_, v)| v),
        Some(&PyRep::Integer(JITA_STATION_ID))
    );
    assert_eq!(buy_row_cols.get(9).map(|(_, v)| v), Some(&PyRep::Integer(5)));
    assert!(buy_row_cols.get(10).and_then(|(_, c)| c.as_i64()).unwrap_or(0) > 0);
    assert_eq!(buy_row_cols.get(11).map(|(_, v)| v), Some(&PyRep::Integer(30)));

    let (pye_item_id, assembled_ship_id) = ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_002).or_default().remove(&3443);
        snap.next_entity_id += 1;
        let item_id = snap.next_entity_id;
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: TRITANIUM_TYPE_ID,
                owner_id: 90_000_002,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 10_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let pye_id = snap.next_entity_id;
        snap.items.insert(
            pye_id,
            InventoryItemRecord {
                item_id: pye_id,
                type_id: 35,
                owner_id: 90_000_002,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let assembled_id = snap.next_entity_id;
        snap.items.insert(
            assembled_id,
            InventoryItemRecord {
                item_id: assembled_id,
                type_id: 606,
                owner_id: 90_000_002,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        (pye_id, assembled_id)
    });

    let assembled_err = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION_ID)),
                    ("typeID", PyRep::Integer(606)),
                    ("itemID", PyRep::Integer(assembled_ship_id)),
                    ("price", PyRep::Real(500_000.0)),
                    ("quantity", PyRep::Integer(1)),
                ])]),
                PyRep::Bool(false),
                PyRep::Integer(90),
                PyRep::Real(100_000.0),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(assembled_err.unwrap_err().code, "CannotSellAssembledItem");

    let sell_price = 25.0;
    let sell_qty = 2_000;
    let s2_bal_before_sell = ctx.db.get_character(90_000_002).unwrap().balance;
    let sell_order = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION_ID)),
                    ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                    ("price", PyRep::Real(sell_price)),
                    ("quantity", PyRep::Integer(sell_qty)),
                ])]),
                PyRep::Bool(false),
                PyRep::Integer(90),
                PyRep::Real(0.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("PlaceMultiSellOrder sell");

    let PyRep::KeyVal(sell_kv) = sell_order else {
        panic!("Expected KeyVal");
    };
    assert_eq!(sell_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));
    let sell_oid = sell_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();
    let sell_broker_isk = sell_kv.get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap();
    let s2_bal_after_sell = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(s2_bal_before_sell.0 - s2_bal_after_sell.0, IskCents::from_f64_isk(sell_broker_isk).0);

    // Verify placed sell order appears in GetOrders sell book with station, range, volRemaining, issueDate, and duration
    let orders_after_sell = ctx
        .dispatch_rpc(&mut s1, "marketProxy", None, "GetOrders", &[PyRep::Integer(TRITANIUM_TYPE_ID as i64)], &kw, None)
        .await
        .expect("GetOrders after sell");
    let PyRep::Tuple(ref books2) = orders_after_sell else { panic!("Expected Tuple") };
    let PyRep::ObjectEx { list: ref ask_rows, .. } = books2[0] else { panic!("Expected CRowset") };
    let sell_row_cols = ask_rows
        .iter()
        .find_map(|r| match r {
            PyRep::DBRow { columns, .. } if columns.get(3).and_then(|(_, c)| c.as_i64()) == Some(sell_oid) => Some(columns),
            _ => None,
        })
        .expect("Placed sell order must appear in GetOrders sell rowset");
    assert_eq!(sell_row_cols.get(0).map(|(_, v)| v), Some(&PyRep::Real(25.0)));
    assert_eq!(sell_row_cols.get(1).map(|(_, v)| v), Some(&PyRep::Real(2_000.0)));
    assert_eq!(sell_row_cols.get(7).map(|(_, v)| v), Some(&PyRep::Integer(JITA_STATION_ID)));
    assert_eq!(sell_row_cols.get(9).map(|(_, v)| v), Some(&PyRep::Integer(32767)));
    assert!(sell_row_cols.get(10).and_then(|(_, c)| c.as_i64()).unwrap_or(0) > 0);
    assert_eq!(sell_row_cols.get(11).map(|(_, v)| v), Some(&PyRep::Integer(90)));

    let s2_hangar_trit = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                i.owner_id == 90_000_002
                    && i.location_id == JITA_STATION_ID
                    && i.flag == InventoryFlag::Hangar
                    && i.type_id == TRITANIUM_TYPE_ID
            })
            .map(|i| i.quantity)
            .sum::<i32>()
    });
    assert_eq!(s2_hangar_trit, 8_000);

    let atomic_fail = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![
                    PyRep::keyval([
                        ("stationID", PyRep::Integer(JITA_STATION_ID)),
                        ("typeID", PyRep::Integer(35)),
                        ("itemID", PyRep::Integer(pye_item_id)),
                        ("price", PyRep::Real(12.0)),
                        ("quantity", PyRep::Integer(200)),
                    ]),
                    PyRep::keyval([
                        ("stationID", PyRep::Integer(JITA_STATION_ID)),
                        ("typeID", PyRep::Integer(606)),
                        ("itemID", PyRep::Integer(assembled_ship_id)),
                        ("price", PyRep::Real(500_000.0)),
                        ("quantity", PyRep::Integer(1)),
                    ]),
                ]),
                PyRep::Bool(false),
                PyRep::Integer(90),
                PyRep::Real(100_000.0),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(atomic_fail.unwrap_err().code, "CannotSellAssembledItem");
    let pye_qty_after_rollback = ctx
        .db
        .with_snapshot(|snap| snap.items[&pye_item_id].quantity);
    assert_eq!(pye_qty_after_rollback, 1_000);

    let corp_err = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION_ID)),
                    ("typeID", PyRep::Integer(35)),
                    ("itemID", PyRep::Integer(pye_item_id)),
                    ("price", PyRep::Real(12.0)),
                    ("quantity", PyRep::Integer(100)),
                ])]),
                PyRep::Bool(true),
                PyRep::Integer(90),
                PyRep::Real(1_000.0),
            ],
            &kw,
            None,
        )
        .await;
    assert!(matches!(
        corp_err.unwrap_err().code.as_str(),
        "CorporationNotFound" | "CorpRoleRequired"
    ));

    let fee_exceeded_err = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION_ID)),
                    ("typeID", PyRep::Integer(35)),
                    ("itemID", PyRep::Integer(pye_item_id)),
                    ("price", PyRep::Real(12.0)),
                    ("quantity", PyRep::Integer(200)),
                ])]),
                PyRep::Bool(false),
                PyRep::Integer(90),
                PyRep::Real(10.0),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(fee_exceeded_err.unwrap_err().code, "BrokerFeeExceeded");

    let multi_sell = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceMultiSellOrder",
            &[
                PyRep::List(
                    (12..=15)
                        .map(|p| {
                            PyRep::keyval([
                                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                                ("typeID", PyRep::Integer(35)),
                                ("itemID", PyRep::Integer(pye_item_id)),
                                ("price", PyRep::Real(p as f64)),
                                ("quantity", PyRep::Integer(200)),
                            ])
                        })
                        .collect(),
                ),
                PyRep::Bool(false),
                PyRep::Integer(90),
                PyRep::Real(400.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("PlaceMultiSellOrder");
    let PyRep::KeyVal(ms_kv) = multi_sell else {
        panic!("Expected KeyVal");
    };
    assert_eq!(ms_kv.get("placedCount"), Some(&PyRep::Integer(4)));
    assert_eq!(ms_kv.get("totalBrokerFeeISK"), Some(&PyRep::Real(400.0)));

    let limit_err = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION_ID)),
                    ("typeID", PyRep::Integer(35)),
                    ("itemID", PyRep::Integer(pye_item_id)),
                    ("price", PyRep::Real(16.0)),
                    ("quantity", PyRep::Integer(100)),
                ])]),
                PyRep::Bool(false),
                PyRep::Integer(90),
                PyRep::Real(1_000.0),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(limit_err.unwrap_err().code, "MaxOpenOrdersReached");

    let cancel_buy = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "CancelCharOrder",
            &[PyRep::Integer(buy_oid)],
            &kw,
            None,
        )
        .await
        .expect("CancelCharOrder buy");
    assert_eq!(cancel_buy, PyRep::Bool(true));

    let s1_bal_after_cancel = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(
        s1_bal_after_cancel.0 - s1_bal_after_buy.0,
        expected_escrow_cents
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(
            16597,
            TrainedSkillRecord {
                type_id: 16597,
                level: 4,
                skill_points: 135_765.0,
            },
        );
    });
    let mt_buy = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(2.0),
                PyRep::Integer(5_000),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(90),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("PlaceBuyOrder with Margin Trading 4");
    let PyRep::KeyVal(mt_kv) = mt_buy else {
        panic!("Expected KeyVal");
    };
    let mt_oid = mt_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();
    let mt_escrow = ctx
        .db
        .with_snapshot(|snap| snap.market_orders[&mt_oid].escrow);
    assert_eq!(mt_escrow, IskCents(316_406));

    // Margin Trading fill balance check:
    // 1) When buyer's wallet cannot cover the un-escrowed 6,835.94 ISK shortfall on fill, the order is cancelled and escrow refunded
    let s1_saved_bal = ctx.db.get_character(90_000_001).unwrap().balance;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().balance = IskCents::from_isk(1_000);
        snap.skills.entry(90_000_002).or_default().insert(
            3443, // Trade V so s2 can place additional orders
            TrainedSkillRecord { type_id: 3443, level: 5, skill_points: 256_000.0 },
        );
    });
    let insolvent_fill = ctx
        .dispatch_rpc(
            &mut s2, "marketProxy", None, "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION_ID)),
                    ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                    ("price", PyRep::Real(2.0)),
                    ("quantity", PyRep::Integer(5_000)),
                ])]),
                PyRep::Bool(false), PyRep::Integer(90), PyRep::Real(0.0),
            ],
            &kw, None,
        )
        .await
        .expect("PlaceMultiSellOrder when buyer is insolvent cancels buyer's Margin Trading order");
    assert_eq!(insolvent_fill.dict_get("filledImmediately").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(ctx.db.with_snapshot(|snap| snap.market_orders[&mt_oid].vol_remaining), 0);
    assert_eq!(
        ctx.db.get_character(90_000_001).unwrap().balance,
        IskCents::from_isk(1_000).checked_add(mt_escrow).unwrap(),
        "Insolvent Margin Trading order cancellation must refund escrowed ISK"
    );
    let resting_sell_from_insolvent = insolvent_fill.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(&mut s2, "marketProxy", None, "CancelCharOrder", &[PyRep::Integer(resting_sell_from_insolvent)], &kw, None).await.unwrap();

    // 2) Restore s1's wallet balance, place a second Margin Trading 4 buy order (4,000 @ 2.0 ISK), and fill it
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().balance = s1_saved_bal;
    });
    let mt_buy2 = ctx
        .dispatch_rpc(
            &mut s1, "marketProxy", None, "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(2.0), PyRep::Integer(4_000), PyRep::Integer(-1),
                PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
            ],
            &kw, None,
        )
        .await
        .expect("Place second Margin Trading buy order");
    let mt_oid2 = mt_buy2.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    let mt_escrow2 = ctx.db.with_snapshot(|snap| snap.market_orders[&mt_oid2].escrow);
    assert_eq!(mt_escrow2, IskCents(253_125)); // 8,000 ISK * 0.75^4 = 2,531.25 ISK
    let s1_bal_before_mt_fill = ctx.db.get_character(90_000_001).unwrap().balance;
    let solvent_fill = ctx
        .dispatch_rpc(
            &mut s2, "marketProxy", None, "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION_ID)),
                    ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                    ("price", PyRep::Real(2.0)),
                    ("quantity", PyRep::Integer(4_000)),
                ])]),
                PyRep::Bool(false), PyRep::Integer(0), PyRep::Real(0.0),
            ],
            &kw, None,
        )
        .await
        .expect("Solvent Margin Trading fill must succeed");
    assert_eq!(solvent_fill.dict_get("filledImmediately").and_then(|v| v.as_bool()), Some(true));
    let s1_bal_after_mt_fill = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(
        s1_bal_before_mt_fill.0 - s1_bal_after_mt_fill.0,
        IskCents::from_isk(8_000).0 - mt_escrow2.0,
        "Un-escrowed remainder (5,468.75 ISK) must be debited from buyer wallet on fill"
    );

    ctx.dispatch_rpc(
        &mut s2,
        "marketProxy",
        None,
        "CancelCharOrder",
        &[PyRep::Integer(sell_oid)],
        &kw,
        None,
    )
    .await
    .expect("CancelCharOrder sell");

    let s2_hangar_after_cancel = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                i.owner_id == 90_000_002
                    && i.location_id == JITA_STATION_ID
                    && i.flag == InventoryFlag::Hangar
                    && i.type_id == TRITANIUM_TYPE_ID
            })
            .map(|i| i.quantity)
            .sum::<i32>()
    });
    assert_eq!(s2_hangar_after_cancel, 6_000);
}
