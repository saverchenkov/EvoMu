//! Phase 6/7 Economy & Market Lifecycle Integration Tests (`E-1`, `E-2`, `E-6`, and Global PLEX Market).
//!
//! Covers:
//! - E-1: Broker fee calculation with Broker Relations skill discount, Sales tax with Accounting skill discount,
//!   4-significant-figure tick size validation, and V24 order modification relist fees (`ModifyCharOrder`)
//! - E-2: Market order matching engine, regional/system/station queries, price history (`GetHistoryForManyTypeIDs`),
//!   order expiry & history (`GetMarketOrderHistory`), corporate orders (`GetCorporationOrders`),
//!   remote market skills (`Procurement`, `Marketing`, `Daytrading`, `Visibility`),
//!   Sleeper blue loot NPC buy orders (`StartupCheck`), and global PLEX (`44992`) market trading
//! - E-6: LP store offerings lookup and exchange



use crate::common;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
    TrainedSkillRecord, TypeId,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_STATION_ID: i64 = 60_003_760;
const TRITANIUM_TYPE_ID: TypeId = 34;
const BROKER_RELATIONS_SKILL: TypeId = 3446;
const ACCOUNTING_SKILL: TypeId = 16622;
const PLEX_TYPE_ID: TypeId = 44_992;

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

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

#[tokio::test]
async fn test_e1_broker_fee_and_sales_tax_skill_discounts() {
    // parity: feat:market.broker-fee, feat:market.sales-tax
    eve_testkit::attest(&["feat:market.broker-fee", "feat:market.sales-tax"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 101, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 102, "other", "other-secret", 90_000_002).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.standings.clear();
        }
        snap.skills
            .entry(90_000_001)
            .or_default()
            .insert(
                BROKER_RELATIONS_SKILL,
                TrainedSkillRecord {
                    type_id: BROKER_RELATIONS_SKILL,
                    level: 0,
                    skill_points: 0.0,
                },
            );
    });

    let order_l0 = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(1.0),
            PyRep::Integer(10_000),
            PyRep::Integer(90),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("PlaceBuyOrder level 0");

    let PyRep::KeyVal(kv_l0) = order_l0 else {
        panic!("Expected KeyVal");
    };
    assert_eq!(kv_l0.get("filledImmediately"), Some(&PyRep::Bool(false)));
    let fee_l0 = kv_l0.get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap();
    assert!((fee_l0 - 300.0).abs() < 0.01);

    let oid_l0 = kv_l0.get("orderID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(oid_l0)])
        .await
        .expect("CancelCharOrder");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills
            .entry(90_000_001)
            .or_default()
            .insert(
                BROKER_RELATIONS_SKILL,
                TrainedSkillRecord {
                    type_id: BROKER_RELATIONS_SKILL,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
    });

    let order_l5 = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(1.0),
            PyRep::Integer(10_000),
            PyRep::Integer(90),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("PlaceBuyOrder level 5");

    let PyRep::KeyVal(kv_l5) = order_l5 else {
        panic!("Expected KeyVal");
    };
    let fee_l5 = kv_l5.get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap();
    assert!((fee_l5 - 150.0).abs() < 0.01);

    let oid_l5 = kv_l5.get("orderID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(oid_l5)])
        .await
        .expect("CancelCharOrder");

    let has_broker_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal
            .iter()
            .any(|j| j.character_id == 90_000_001 && j.entry_type == "MarketBrokerFee")
    });
    assert!(has_broker_journal);

    let buy_order = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(1.0),
            PyRep::Integer(200),
            PyRep::Integer(90),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("Place buy order");
    let PyRep::KeyVal(buy_kv) = buy_order else {
        panic!("Expected KeyVal");
    };
    assert_eq!(buy_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));

    ctx.db.with_mut_snapshot(|snap| {
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
                quantity: 200,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.skills
            .entry(90_000_002)
            .or_default()
            .insert(
                ACCOUNTING_SKILL,
                TrainedSkillRecord {
                    type_id: ACCOUNTING_SKILL,
                    level: 0,
                    skill_points: 0.0,
                },
            );
    });

    let s2_bal_before = ctx.db.get_character(90_000_002).unwrap().balance;
    let sell_l0 = rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                ("price", PyRep::Real(1.0)),
                ("quantity", PyRep::Integer(100)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect("PlaceMultiSellOrder sell level 0");

    let PyRep::KeyVal(sell_l0_kv) = sell_l0 else {
        panic!("Expected KeyVal");
    };
    assert_eq!(sell_l0_kv.get("filledImmediately"), Some(&PyRep::Bool(true)));
    let tax_l0 = sell_l0_kv.get("salesTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!((tax_l0 - 14.10).abs() < 0.01);

    let broker_l0 = sell_l0_kv.get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap();
    let s2_bal_after = ctx.db.get_character(90_000_002).unwrap().balance;
    let net_gain = (s2_bal_after.0 - s2_bal_before.0) as f64 / 100.0;
    let expected_gain = 188.0 - 14.10 - broker_l0;
    assert!((net_gain - expected_gain).abs() < 0.02);

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills
            .entry(90_000_002)
            .or_default()
            .insert(
                ACCOUNTING_SKILL,
                TrainedSkillRecord {
                    type_id: ACCOUNTING_SKILL,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
    });

    let sell_l5 = rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                ("price", PyRep::Real(1.0)),
                ("quantity", PyRep::Integer(100)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect("PlaceMultiSellOrder sell level 5");

    let PyRep::KeyVal(sell_l5_kv) = sell_l5 else {
        panic!("Expected KeyVal");
    };
    assert_eq!(sell_l5_kv.get("filledImmediately"), Some(&PyRep::Bool(true)));
    let tax_l5 = sell_l5_kv.get("salesTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!((tax_l5 - 6.35).abs() < 0.01);
}

#[tokio::test]
async fn test_e2_market_order_matching_queries_and_cancellation() {
    // parity: feat:market.cancel-order, feat:market.price-history
    eve_testkit::attest(&["feat:market.cancel-order", "feat:market.price-history"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 201, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 202, "other", "other-secret", 90_000_002).await;

    let orders_rep = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "GetOrders",
        &[PyRep::Integer(TRITANIUM_TYPE_ID as i64)],
    )
    .await
    .expect("GetOrders");
    assert!(matches!(orders_rep, PyRep::Tuple(_)));

    let asks_rep = rpc(&ctx, &mut s1, "marketProxy", "GetStationAsks", &[])
        .await
        .expect("GetStationAsks");
    assert!(asks_rep.is_cindexed_rowset());

    let region_rep = rpc(&ctx, &mut s1, "marketProxy", "GetRegionBest", &[])
        .await
        .expect("GetRegionBest");
    assert!(region_rep.is_cindexed_rowset());

    let hist_rep = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "GetOldPriceHistory",
        &[PyRep::Integer(TRITANIUM_TYPE_ID as i64)],
    )
    .await
    .expect("GetOldPriceHistory");
    assert!(hist_rep.is_crowset());

    let new_hist_rep = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "GetNewPriceHistory",
        &[PyRep::Integer(TRITANIUM_TYPE_ID as i64)],
    )
    .await
    .expect("GetNewPriceHistory");
    assert!(new_hist_rep.is_crowset());

    let s1_trit_before = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                i.owner_id == 90_000_001
                    && i.location_id == JITA_STATION_ID
                    && i.flag == InventoryFlag::Hangar
                    && i.type_id == TRITANIUM_TYPE_ID
            })
            .map(|i| i.quantity)
            .sum::<i32>()
    });
    assert!(s1_trit_before >= 1_000);

    let sell_order = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                ("price", PyRep::Real(15.00)),
                ("quantity", PyRep::Integer(1_000)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect("Place sell order");
    let PyRep::KeyVal(sell_kv) = sell_order else {
        panic!("Expected KeyVal");
    };
    assert_eq!(sell_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));
    let sell_oid = sell_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();

    let s1_trit_after_place = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                i.owner_id == 90_000_001
                    && i.location_id == JITA_STATION_ID
                    && i.flag == InventoryFlag::Hangar
                    && i.type_id == TRITANIUM_TYPE_ID
            })
            .map(|i| i.quantity)
            .sum::<i32>()
    });
    assert_eq!(s1_trit_after_place, s1_trit_before - 1_000);

    let char_orders = rpc(&ctx, &mut s1, "marketProxy", "GetCharOrders", &[])
        .await
        .expect("GetCharOrders");
    let PyRep::Rowset { rows: co_rows, .. } = char_orders else {
        panic!("Expected Rowset");
    };
    assert!(co_rows
        .iter()
        .any(|row| row.first().and_then(|v| v.as_i64()) == Some(sell_oid)));

    rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "CancelCharOrder",
        &[PyRep::Integer(sell_oid)],
    )
    .await
    .expect("CancelCharOrder");

    let s1_trit_after_cancel = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                i.owner_id == 90_000_001
                    && i.location_id == JITA_STATION_ID
                    && i.flag == InventoryFlag::Hangar
                    && i.type_id == TRITANIUM_TYPE_ID
            })
            .map(|i| i.quantity)
            .sum::<i32>()
    });
    assert_eq!(s1_trit_after_cancel, s1_trit_before);

    let resting_sell = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                ("price", PyRep::Real(3.00)),
                ("quantity", PyRep::Integer(100)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect("Place resting sell order");
    let PyRep::KeyVal(r_sell_kv) = resting_sell else {
        panic!("Expected KeyVal");
    };
    let resting_oid = r_sell_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();

    let s2_trit_before = ctx.db.with_snapshot(|snap| {
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

    let match_buy = rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(3.00),
            PyRep::Integer(40),
            PyRep::Integer(90),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("Place matching buy order");
    let PyRep::KeyVal(match_kv) = match_buy else {
        panic!("Expected KeyVal");
    };
    assert_eq!(match_kv.get("filledImmediately"), Some(&PyRep::Bool(true)));

    let s2_trit_after = ctx.db.with_snapshot(|snap| {
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
    assert_eq!(s2_trit_after, s2_trit_before + 40);

    let rem_vol = ctx
        .db
        .with_snapshot(|snap| snap.market_orders.get(&resting_oid).map(|o| o.vol_remaining))
        .unwrap();
    assert_eq!(rem_vol, 60);

    // Verify OnOwnOrdersChanged was pushed to both seller (90_000_001) and buyer (90_000_002)
    let p1_order_notifs = ctx.drain_notifications_for_char(90_000_001);
    let p2_order_notifs = ctx.drain_notifications_for_char(90_000_002);
    assert!(p1_order_notifs.iter().any(|n| n.event == "OnOwnOrdersChanged"));
    assert!(p2_order_notifs.iter().any(|n| n.event == "OnOwnOrdersChanged"));

    // Buy for 100 units against a resting sell of 40 units fills 40 immediately and posts resting buy for 60 units
    let _ = rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(resting_oid)]).await.expect("CancelCharOrder");
    let sell_40 = rpc(&ctx, &mut s1, "marketProxy", "PlaceMultiSellOrder", &[
        PyRep::List(vec![PyRep::keyval([("stationID", PyRep::Integer(JITA_STATION_ID)), ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)), ("price", PyRep::Real(3.00)), ("quantity", PyRep::Integer(40))])]),
        PyRep::Bool(false), PyRep::Integer(90), PyRep::Real(0.0),
    ]).await.expect("Place 40-unit sell order");
    let PyRep::KeyVal(s40_kv) = sell_40 else { panic!("Expected KeyVal"); };
    let sell_40_oid = s40_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();

    let buy_100 = rpc(&ctx, &mut s2, "marketProxy", "PlaceBuyOrder", &[
        PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Real(3.00),
        PyRep::Integer(100), PyRep::Integer(90), PyRep::Integer(1),
    ]).await.expect("Place 100-unit buy order against 40-unit sell");
    let PyRep::KeyVal(b100_kv) = buy_100 else { panic!("Expected KeyVal"); };
    assert_eq!(b100_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));
    let buy_60_oid = b100_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(ctx.db.with_snapshot(|snap| snap.market_orders.get(&sell_40_oid).map(|o| o.vol_remaining)), Some(0));
    assert_eq!(ctx.db.with_snapshot(|snap| snap.market_orders.get(&buy_60_oid).map(|o| o.vol_remaining)), Some(60));
    rpc(&ctx, &mut s2, "marketProxy", "CancelCharOrder", &[PyRep::Integer(buy_60_oid)]).await.expect("Cancel resting 60-unit buy order");

    let s2_bal_before_buy = ctx.db.get_character(90_000_002).unwrap().balance;
    let limit_buy = rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(2.00),
            PyRep::Integer(500),
            PyRep::Integer(90),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("Place limit buy order");
    let PyRep::KeyVal(l_buy_kv) = limit_buy else {
        panic!("Expected KeyVal");
    };
    let l_buy_oid = l_buy_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();
    let l_buy_fee = l_buy_kv.get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap();

    let s2_bal_after_buy = ctx.db.get_character(90_000_002).unwrap().balance;
    let deducted = (s2_bal_before_buy.0 - s2_bal_after_buy.0) as f64 / 100.0;
    assert!((deducted - (1000.0 + l_buy_fee)).abs() < 0.02);

    rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "CancelCharOrder",
        &[PyRep::Integer(l_buy_oid)],
    )
    .await
    .expect("Cancel buy order");

    let s2_bal_after_cancel = ctx.db.get_character(90_000_002).unwrap().balance;
    let refunded = (s2_bal_after_cancel.0 - s2_bal_after_buy.0) as f64 / 100.0;
    assert!((refunded - 1000.0).abs() < 0.01);

    let has_cancel_refund = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal
            .iter()
            .any(|j| j.character_id == 90_000_002 && j.entry_type == "MarketOrderCancelRefund")
    });
    assert!(has_cancel_refund);
}

#[tokio::test]
async fn test_market_instant_matching() {
    // parity: feat:market.instant-buy, feat:market.instant-sell
    eve_testkit::attest(&["feat:market.instant-buy", "feat:market.instant-sell"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 203, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 204, "other", "other-secret", 90_000_002).await;

    let s2_trit_id = ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_002).or_default().remove(&16622);
        snap.market_orders
            .retain(|_, o| o.type_id != TRITANIUM_TYPE_ID);
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
                quantity: 5_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        item_id
    });

    let sell_ask = rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![
                PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION_ID)),
                    ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                    ("itemID", PyRep::Integer(s2_trit_id)),
                    ("price", PyRep::Real(10.0)),
                    ("quantity", PyRep::Integer(1_000)),
                ]),
                PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION_ID)),
                    ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                    ("itemID", PyRep::Integer(s2_trit_id)),
                    ("price", PyRep::Real(12.0)),
                    ("quantity", PyRep::Integer(500)),
                ]),
            ]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect("Place resting sell orders");

    let PyRep::KeyVal(ask_kv) = sell_ask else {
        panic!("Expected KeyVal");
    };
    assert_eq!(ask_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));
    assert_eq!(ask_kv.get("placedCount"), Some(&PyRep::Integer(2)));

    let s2_bal_before_fill = ctx.db.get_character(90_000_002).unwrap().balance;

    let instant_buy = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(10.0),
            PyRep::Integer(500),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("Instant PlaceBuyOrder");

    let PyRep::KeyVal(ibuy_kv) = instant_buy else {
        panic!("Expected KeyVal");
    };
    assert_eq!(ibuy_kv.get("filledImmediately"), Some(&PyRep::Bool(true)));

    let s2_bal_after_fill = ctx.db.get_character(90_000_002).unwrap().balance;
    let seller_net_isk = (s2_bal_after_fill.0 - s2_bal_before_fill.0) as f64 / 100.0;
    assert!((seller_net_isk - 4_625.0).abs() < 0.02);

    let multi_buy = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "BuyMultipleItems",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                ("quantity", PyRep::Integer(800)),
                ("price", PyRep::Real(12.0)),
                ("orderRange", PyRep::Integer(-1)),
                ("minVolume", PyRep::Integer(1)),
                ("duration", PyRep::Integer(0)),
            ])]),
        ],
    )
    .await
    .expect("BuyMultipleItems");
    let PyRep::KeyVal(mb_kv) = multi_buy else {
        panic!("Expected KeyVal");
    };
    assert_eq!(mb_kv.get("purchasedCount"), Some(&PyRep::Integer(1)));
    assert_eq!(mb_kv.get("totalQuantity"), Some(&PyRep::Integer(800)));
    assert_eq!(mb_kv.get("totalCostISK"), Some(&PyRep::Real(8_600.0)));
    assert!(matches!(mb_kv.get("ordersCreated"), Some(PyRep::List(l)) if l.len() == 2));

    let s1_hangar_trit = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                i.owner_id == 90_000_001
                    && i.location_id == JITA_STATION_ID
                    && i.flag == InventoryFlag::Hangar
                    && i.type_id == TRITANIUM_TYPE_ID
            })
            .map(|i| i.quantity)
            .sum::<i32>()
    });
    assert!(s1_hangar_trit >= 1_300);

    let resting_bid = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(1.0),
            PyRep::Integer(400),
            PyRep::Integer(32767),
            PyRep::Integer(300),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("Place resting buy order");
    let PyRep::KeyVal(bid_kv) = resting_bid else {
        panic!("Expected KeyVal");
    };
    assert_eq!(bid_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));

    let under_min_err = rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                ("itemID", PyRep::Integer(s2_trit_id)),
                ("price", PyRep::Real(1.0)),
                ("quantity", PyRep::Integer(200)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(0),
            PyRep::Real(0.0),
        ],
    )
    .await;
    assert_eq!(under_min_err.unwrap_err().code, "ImmediateOrderUnfilled");

    let s2_bal_before_isell = ctx.db.get_character(90_000_002).unwrap().balance;
    let instant_sell = rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                ("itemID", PyRep::Integer(s2_trit_id)),
                ("price", PyRep::Real(1.0)),
                ("quantity", PyRep::Integer(400)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(0),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect("Instant sell order");
    let PyRep::KeyVal(isell_kv) = instant_sell else {
        panic!("Expected KeyVal");
    };
    assert_eq!(isell_kv.get("filledImmediately"), Some(&PyRep::Bool(true)));
    let s2_bal_after_isell = ctx.db.get_character(90_000_002).unwrap().balance;
    assert!(s2_bal_after_isell.0 > s2_bal_before_isell.0);
}

#[tokio::test]
async fn test_market_modify_relist_tick_size_corp_remote_expiry_sleeper_and_plex() {
    // parity: feat:market.modify-order, feat:market.remote-orders, feat:market.corp-orders, feat:market.plex-market, feat:market.npc-buy-orders-sleeper-loot, rpc:marketProxy.ModifyCharOrder
    eve_testkit::attest(&["feat:market.modify-order", "feat:market.remote-orders", "feat:market.corp-orders", "feat:market.plex-market", "feat:market.npc-buy-orders-sleeper-loot"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 210, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 211, "other", "other-secret", 90_000_002).await;

    // 1. GetMarketGroups, GetSystemAsks, GetHistoryForManyTypeIDs
    let mgs = rpc(&ctx, &mut s1, "marketProxy", "GetMarketGroups", &[])
        .await
        .expect("GetMarketGroups");
    assert!(matches!(mgs, PyRep::Rowset { ref rows, .. } if !rows.is_empty()));

    let sys_asks = rpc(&ctx, &mut s1, "marketProxy", "GetSystemAsks", &[])
        .await
        .expect("GetSystemAsks");
    assert!(sys_asks.is_cindexed_rowset());

    let many_hist = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "GetHistoryForManyTypeIDs",
        &[PyRep::List(vec![PyRep::Integer(34), PyRep::Integer(35)])],
    )
    .await
    .expect("GetHistoryForManyTypeIDs");
    let PyRep::Dict(hist_entries) = many_hist else {
        panic!("Expected Dict from GetHistoryForManyTypeIDs");
    };
    assert_eq!(hist_entries.len(), 2);

    // 2. StartupCheck & Sleeper Blue Loot NPC buy orders (`30744` @ 200k, `30745` @ 500k, `30746` @ 1.5M, `30747` @ 5M ISK)
    let startup = rpc(&ctx, &mut s1, "marketProxy", "StartupCheck", &[])
        .await
        .expect("StartupCheck");
    let PyRep::KeyVal(st_kv) = startup else {
        panic!("Expected KeyVal");
    };
    assert_eq!(st_kv.get("sleeperBuyOrdersSeeded"), Some(&PyRep::Bool(true)));

    // Give Pilot 1 a Sleeper Neural Network Analyzer (`30744`) and sell into the seeded 200,000 ISK NPC buy order
    let sleeper_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().remove(&16622); // Accounting 0 -> 7.5% sales tax
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 30_744,
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        id
    });
    let s1_bal_before_sleeper = ctx.db.get_character(90_000_001).unwrap().balance;
    let sell_sleeper = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(30_744)),
                ("itemID", PyRep::Integer(sleeper_item_id)),
                ("price", PyRep::Real(200_000.0)),
                ("quantity", PyRep::Integer(2)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(0),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect("Sell Sleeper blue loot to NPC buy order");
    let PyRep::KeyVal(sl_kv) = sell_sleeper else {
        panic!("Expected KeyVal");
    };
    assert_eq!(sl_kv.get("filledImmediately"), Some(&PyRep::Bool(true)));
    // 2 * 200,000 = 400,000 ISK gross; minus 7.5% sales tax (30,000 ISK) = 370,000 ISK net
    let s1_bal_after_sleeper = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(
        s1_bal_after_sleeper.0 - s1_bal_before_sleeper.0,
        37_000_000
    );

    // 3. 4-Significant-Figure Tick Size (`feat:market.tick-size-4-sig-figs`) & 5-Minute Cooldown + Relist Fee (`feat:market.order-modify-relist-fee`)
    let buy_res = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(1.00),
            PyRep::Integer(1_000),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("PlaceBuyOrder for ModifyCharOrder test");
    let PyRep::KeyVal(b_kv) = buy_res else {
        panic!("Expected KeyVal");
    };
    let mod_oid = b_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();

    // Invalid 5-significant-figure price (`123.45`) must fail with InvalidPriceTickSize
    let tick_err = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "ModifyCharOrder",
        &[PyRep::Integer(mod_oid), PyRep::Real(123.45)],
    )
    .await
    .unwrap_err();
    assert_eq!(tick_err.code, "InvalidPriceTickSize");

    // Modifying within 300s cooldown must fail with OrderModificationCooldown
    let cd_err = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "ModifyCharOrder",
        &[PyRep::Integer(mod_oid), PyRep::Real(1.50)],
    )
    .await
    .unwrap_err();
    assert_eq!(cd_err.code, "OrderModificationCooldown");

    // Backdate order issue_unix by 301s and train Advanced Broker Relations (16597) to level 5
    let now_unix = ctx.now_unix();
    let meta_nid = -(4_000_000_000_000_i64.saturating_add(mod_oid.abs()));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(note) = snap.owner_notes.get_mut(&meta_nid) {
            note.note = format!(
                "{{\"range\":-1,\"duration\":90,\"issue_unix\":{}}}",
                now_unix.saturating_sub(400)
            );
        }
        snap.skills.entry(90_000_001).or_default().insert(
            16597,
            TrainedSkillRecord {
                type_id: 16597,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });

    let mod_ok = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "ModifyCharOrder",
        &[PyRep::Integer(mod_oid), PyRep::Real(1.50)],
    )
    .await
    .expect("ModifyCharOrder after cooldown");
    let PyRep::KeyVal(mod_kv) = mod_ok else {
        panic!("Expected KeyVal");
    };
    assert_eq!(mod_kv.get("newPrice"), Some(&PyRep::Real(1.50)));
    assert_eq!(mod_kv.get("relistFeeISK"), Some(&PyRep::Real(100.0))); // 100 ISK minimum relist fee floor

    // 4. Remote Order Modification (`Daytrading` `16595` & `Visibility` `3447`) (`feat:market.remote-orders`)
    // Move Pilot 1 to another station in Jita (`60_003_766`) and backdate cooldown
    s1.station_id = Some(60_003_766);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.station_id = Some(60_003_766); }
        if let Some(note) = snap.owner_notes.get_mut(&meta_nid) {
            note.note = format!("{{\"range\":-1,\"duration\":90,\"issue_unix\":{}}}", now_unix.saturating_sub(400));
        }
        snap.skills.entry(90_000_001).or_default().remove(&16595); // Daytrading 0
        snap.skills.entry(90_000_001).or_default().remove(&3447); // Visibility 0
    });
    let dt_err = rpc(&ctx, &mut s1, "marketProxy", "ModifyCharOrder", &[PyRep::Integer(mod_oid), PyRep::Real(1.60)]).await.unwrap_err();
    assert_eq!(dt_err.code, "DaytradingSkillRequired");

    // Train Daytrading (16595) = 1 (same solar system); requesting buy range 5 with Visibility = 0 fails with VisibilitySkillRequired
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(
            16595,
            TrainedSkillRecord { type_id: 16595, level: 1, skill_points: 1_000.0 },
        );
    });
    let mut vis_kw = HashMap::new();
    vis_kw.insert("newRange".into(), PyRep::Integer(5));
    let vis_err = ctx.dispatch_rpc(&mut s1, "marketProxy", None, "ModifyCharOrder", &[PyRep::Integer(mod_oid), PyRep::Real(1.60)], &vis_kw, None).await.unwrap_err();
    assert_eq!(vis_err.code, "VisibilitySkillRequired");

    // Train Visibility (3447) = 2 (allows range 5) -> remote modification succeeds!
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(
            3447,
            TrainedSkillRecord { type_id: 3447, level: 2, skill_points: 8_000.0 },
        );
    });
    let remote_mod_ok = ctx.dispatch_rpc(&mut s1, "marketProxy", None, "ModifyCharOrder", &[PyRep::Integer(mod_oid), PyRep::Real(1.60)], &vis_kw, None).await.expect("Remote ModifyCharOrder with Daytrading 1 and Visibility 2");
    let PyRep::KeyVal(rm_kv) = remote_mod_ok else { panic!("Expected KeyVal"); };
    assert_eq!(rm_kv.get("range"), Some(&PyRep::Integer(5)));
    assert_eq!(rm_kv.get("maxDaytradingJumps"), Some(&PyRep::Integer(0)));
    assert_eq!(rm_kv.get("maxVisibilityJumps"), Some(&PyRep::Integer(5)));
    s1.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.station_id = Some(JITA_STATION_ID); }
    });

    // 5. Corporate Orders (`GetCorporationOrders`, `accountKey`, `InventoryFlag::Deliveries`) (`feat:market.corp-orders`)
    let (corp_id, corp_buy_oid) = ctx.db.with_mut_snapshot(|snap| {
        let cid = snap.characters[&90_000_001].corporation_id;
        let mut corp = CorporationRecord {
            corporation_id: cid,
            name: "Trader Corp".into(),
            ticker: "TRDR".into(),
            ceo_id: 90_000_001,
            master_wallet: IskCents::from_isk(10_000_000),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        if let Some(div3) = corp.wallet_divisions.iter_mut().find(|d| d.division_id == 3) {
            div3.balance = IskCents::from_isk(5_000_000);
        }
        snap.corporations.insert(cid, corp);

        // Seed a corporate buy order and a resting ask order at 2.00 ISK
        snap.next_entity_id += 1;
        let c_oid = snap.next_entity_id;
        snap.market_orders.insert(
            c_oid,
            MarketOrderRecord {
                order_id: c_oid,
                owner_id: cid,
                station_id: JITA_STATION_ID,
                region_id: 10_000_002,
                type_id: 35, // Pyerite
                is_bid: true,
                price: IskCents::from_f64_isk(1.00),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_f64_isk(100.0),
            },
        );
        snap.next_entity_id += 1;
        let ask_oid = snap.next_entity_id;
        snap.market_orders.insert(
            ask_oid,
            MarketOrderRecord {
                order_id: ask_oid,
                owner_id: 90_000_002,
                station_id: JITA_STATION_ID,
                region_id: 10_000_002,
                type_id: 35,
                is_bid: false,
                price: IskCents::from_f64_isk(2.00),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        (cid, c_oid)
    });

    let corp_orders = rpc(&ctx, &mut s1, "marketProxy", "GetCorporationOrders", &[])
        .await
        .expect("GetCorporationOrders");
    let PyRep::Rowset { rows: co_rows, .. } = corp_orders else {
        panic!("Expected Rowset from GetCorporationOrders");
    };
    assert!(!co_rows.is_empty());

    // Modify corporate buy order with accountKey=1002 (division 3) to 2.00 ISK -> crosses ask and delivers 100 Pyerite to corp Deliveries (`flag = 173`)
    let mut corp_kw = HashMap::new();
    corp_kw.insert("accountKey".into(), PyRep::Integer(1002));
    let corp_mod = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "ModifyCharOrder",
            &[PyRep::Integer(corp_buy_oid), PyRep::Real(2.00)],
            &corp_kw,
            None,
        )
        .await
        .expect("Modify corporate buy order");
    let PyRep::KeyVal(cm_kv) = corp_mod else {
        panic!("Expected KeyVal");
    };
    assert_eq!(cm_kv.get("accountKey"), Some(&PyRep::Integer(1002)));
    assert_eq!(cm_kv.get("corpDeliveredQty"), Some(&PyRep::Integer(100)));
    let in_corp_deliveries = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|i| {
            i.owner_id == corp_id
                && i.location_id == JITA_STATION_ID
                && i.flag == InventoryFlag::Deliveries
                && i.type_id == 35
                && i.quantity == 100
        })
    });
    assert!(in_corp_deliveries, "Matched corporate buy items must be delivered to InventoryFlag::Deliveries");

    // 6. Order Expiry Sweep & GetMarketOrderHistory (`feat:market.order-expiry-history`, `rpc:marketProxy.GetMarketOrderHistory`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(note) = snap.owner_notes.get_mut(&meta_nid) {
            note.note = "{\"range\":5,\"duration\":1,\"issue_unix\":1}".into(); // expired!
        }
    });
    let hist_orders = rpc(&ctx, &mut s1, "marketProxy", "GetMarketOrderHistory", &[PyRep::Bool(true)])
        .await
        .expect("GetMarketOrderHistory");
    let PyRep::Rowset { rows: hist_rows, .. } = hist_orders else {
        panic!("Expected Rowset from GetMarketOrderHistory");
    };
    // Verify mod_oid was swept as Expired (`orderState = 4`) and refunded escrow
    let expired_row = hist_rows
        .iter()
        .find(|r| r.first().and_then(|v| v.as_i64()) == Some(mod_oid))
        .expect("Expired order must appear in GetMarketOrderHistory");
    assert_eq!(expired_row.get(11), Some(&PyRep::Integer(4))); // orderState 4 = Expired

    // 7. Global PLEX (`44992`) Market (`feat:plex.market-trading`)
    ctx.db.with_mut_snapshot(|snap| {
        let cur = snap.plex_vault_balance(90_000_001);
        snap.adjust_plex_vault(90_000_001, 500 - cur).expect("account PLEX vault");
    });
    // Place global PLEX sell order for 100 PLEX at 5,100,000 ISK (4 sig figs)
    let plex_sell = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlacePlexSellOrder",
        &[
            PyRep::Real(5_100_000.0),
            PyRep::Integer(100),
            PyRep::Integer(90),
        ],
    )
    .await
    .expect("PlacePlexSellOrder");
    let PyRep::KeyVal(ps_kv) = plex_sell else {
        panic!("Expected KeyVal");
    };
    let plex_oid = ps_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(ps_kv.get("plexRemainingVault"), Some(&PyRep::Integer(400)));

    let plex_best = rpc(&ctx, &mut s2, "marketProxy", "GetPlexBest", &[])
        .await
        .expect("GetPlexBest");
    let PyRep::KeyVal(pb_kv) = plex_best else {
        panic!("Expected KeyVal");
    };
    assert_eq!(pb_kv.get("typeID"), Some(&PyRep::Integer(PLEX_TYPE_ID as i64)));
    assert!(pb_kv.get("bestAskPrice").and_then(|v| v.as_f64()).unwrap() > 0.0);

    let plex_orders = rpc(&ctx, &mut s2, "marketProxy", "GetPlexOrders", &[])
        .await
        .expect("GetPlexOrders");
    assert!(matches!(plex_orders, PyRep::Tuple(_)));

    let plex_hist = rpc(&ctx, &mut s2, "marketProxy", "GetPlexHistory", &[])
        .await
        .expect("GetPlexHistory");
    assert!(plex_hist.is_crowset());
    let _ = rpc(&ctx, &mut s2, "marketProxy", "GetPlexNewPriceHistory", &[]).await.unwrap();
    let _ = rpc(&ctx, &mut s2, "marketProxy", "GetPlexOldPriceHistory", &[]).await.unwrap();

    // Backdate PLEX order issue_unix and modify via ModifyPlexCharOrder
    let plex_meta_nid = -(4_000_000_000_000_i64.saturating_add(plex_oid.abs()));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(note) = snap.owner_notes.get_mut(&plex_meta_nid) {
            note.note = format!(
                "{{\"range\":32767,\"duration\":90,\"issue_unix\":{}}}",
                now_unix.saturating_sub(400)
            );
        }
    });
    let mod_plex = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "ModifyPlexCharOrder",
        &[PyRep::Integer(plex_oid), PyRep::Real(5_200_000.0)],
    )
    .await
    .expect("ModifyPlexCharOrder");
    let PyRep::KeyVal(mp_kv) = mod_plex else {
        panic!("Expected KeyVal");
    };
    assert_eq!(mp_kv.get("newPrice"), Some(&PyRep::Real(5_200_000.0)));

    // Global PLEX buy order from another station (`60_003_766`) matches globally and credits buyer's PLEX Vault
    s2.station_id = Some(60_003_766);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.station_id = Some(60_003_766);
            ch.balance = IskCents::from_isk(1_000_000_000);
        }
        let cur = snap.plex_vault_balance(90_000_002);
        snap.adjust_plex_vault(90_000_002, -cur).expect("account PLEX vault");
    });
    let plex_buy = rpc(&ctx, &mut s2, "marketProxy", "PlaceBuyOrder", &[
        PyRep::Integer(60_003_766), PyRep::Integer(PLEX_TYPE_ID as i64), PyRep::Real(5_200_000.0),
        PyRep::Integer(40), PyRep::Integer(32767), PyRep::Integer(1), PyRep::Integer(0), PyRep::Bool(false),
    ]).await.expect("Global PLEX buy order");
    let PyRep::KeyVal(pbuy_kv) = plex_buy else { panic!("Expected KeyVal"); };
    assert_eq!(pbuy_kv.get("filledImmediately"), Some(&PyRep::Bool(true)));
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(90_000_002)), 40);
    let plex_in_hangar = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|i| i.owner_id == 90_000_002 && i.type_id == PLEX_TYPE_ID)
    });
    assert!(!plex_in_hangar, "Bought PLEX settles into the PLEX Vault, not the item hangar");

    // Cancelling remaining 60 PLEX sell order returns 60 PLEX to seller's PLEX Vault (400 + 60 = 460)
    rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(plex_oid)]).await.expect("Cancel PLEX sell order");
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(90_000_001)), 460);
    assert!(
        !ctx.db.with_snapshot(|snap| snap.items.values().any(|i| i.owner_id == 90_000_001 && i.type_id == PLEX_TYPE_ID)),
        "Cancelled PLEX returns only to the PLEX Vault, not the hangar"
    );

    // 8. NPC Seed Orders (`feat:market.npc-seed-orders`): Skillbooks (cat 16) & Blueprints (cat 9) never deplete
    eve_server::world_seeder::seed_dual_anchor_markets(&ctx.db, &ctx.sde);
    let (skillbook_oid, skillbook_tid, skillbook_price) = ctx.db.with_snapshot(|snap| {
        snap.market_orders.values().filter(|o| {
            o.is_npc && !o.is_bid && o.station_id == JITA_STATION_ID
                && ctx.sde.types.get(&o.type_id).map(|t| t.category_id == 16).unwrap_or(false)
        }).min_by_key(|o| (o.price, o.order_id)).map(|o| (o.order_id, o.type_id, o.price.to_wire_f64())).expect("Seeded NPC skillbook sell order must exist in Jita")
    });
    let buy_sb = rpc(&ctx, &mut s1, "marketProxy", "PlaceBuyOrder", &[
        PyRep::Integer(JITA_STATION_ID), PyRep::Integer(skillbook_tid as i64), PyRep::Real(skillbook_price),
        PyRep::Integer(5), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(0), PyRep::Bool(false),
    ]).await.expect("Buy 5 skillbooks from NPC seed order");
    let PyRep::KeyVal(sb_kv) = buy_sb else { panic!("Expected KeyVal"); };
    assert_eq!(sb_kv.get("filledImmediately"), Some(&PyRep::Bool(true)));
    assert_eq!(
        ctx.db.with_snapshot(|snap| snap.market_orders.get(&skillbook_oid).map(|o| o.vol_remaining)),
        Some(1_000),
        "NPC skillbook sell order must replenish and not deplete"
    );

    // 9. Order Limits (`feat:market.order-limits`) & Structure Markets (`feat:market.structure-markets`)
    let structure_id = 1_030_000_000_999_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.owner_id != 90_000_001);
        let sk = snap.skills.entry(90_000_001).or_default();
        sk.remove(&3443); // Trade 0 -> base 5 open orders
        sk.remove(&3444); // Retail 0
        sk.remove(&16596); // Wholesale 0
        sk.remove(&18580); // Tycoon 0
        snap.structures.insert(structure_id, eve_db::UpwellStructureRecord {
            structure_id, type_id: 35832, owner_corp_id: corp_id, solar_system_id: 30_000_142,
            name: "Jita Market Citadel".into(), position: [0.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 100, shield_hp: 1000.0, armor_hp: 1000.0, hull_hp: 1000.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0,
        });
    });
    for i in 0..5 {
        rpc(&ctx, &mut s1, "marketProxy", "PlaceBuyOrder", &[
            PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(1.00 + (i as f64) * 0.10), PyRep::Integer(10), PyRep::Integer(-1),
            PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
        ]).await.expect("Place open order within base limit 5");
    }
    let sixth_err = rpc(&ctx, &mut s1, "marketProxy", "PlaceBuyOrder", &[
        PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Real(1.60),
        PyRep::Integer(10), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
    ]).await.unwrap_err();
    assert_eq!(sixth_err.code, "MaxOpenOrdersReached");

    // Train Trade (3443) = 1 (+4 orders -> 9 max) and place order at Upwell structure (`feat:market.structure-markets`)
    s1.station_id = None;
    s1.structure_id = Some(structure_id);
    let corp_bal_before_str = ctx.db.with_snapshot(|snap| snap.corporations[&corp_id].master_wallet);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(structure_id);
        }
        snap.skills.entry(90_000_001).or_default().insert(
            3443,
            TrainedSkillRecord { type_id: 3443, level: 1, skill_points: 250.0 },
        );
    });
    let str_buy = rpc(&ctx, &mut s1, "marketProxy", "PlaceBuyOrder", &[
        PyRep::Integer(structure_id), PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Real(2.00),
        PyRep::Integer(10_000), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
    ]).await.expect("PlaceBuyOrder at Upwell structure after training Trade 1");
    let PyRep::KeyVal(sb_order_kv) = str_buy else { panic!("Expected KeyVal"); };
    let str_fee = sb_order_kv.get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap();
    assert!((str_fee - 300.0).abs() < 0.01); // 1.5% default structure broker rate on 20,000 ISK = 300 ISK
    let corp_bal_after_str = ctx.db.with_snapshot(|snap| snap.corporations[&corp_id].master_wallet);
    assert_eq!(corp_bal_after_str.0 - corp_bal_before_str.0, 15_000); // 50% (150.00 ISK) to structure owner corp wallet
}

#[tokio::test]
async fn test_e6_lp_store_offerings_and_exchange() {
    // parity: feat:lp.lp-balance, feat:lp.lp-store-offers, feat:lp.lp-store-buy
    eve_testkit::attest(&["feat:lp.lp-balance", "feat:lp.lp-store-offers", "feat:lp.lp-store-buy"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 601, "demo", "demo-secret", 90_000_001).await;
    let caldari_navy_corp_id = 1_000_035;

    let offers_rep = rpc(&ctx, &mut s1, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(caldari_navy_corp_id)])
        .await.expect("GetAvailableOffersFromCorp");
    let PyRep::List(offers_list) = offers_rep else { panic!("Expected List"); };
    assert!(!offers_list.is_empty());
    assert!(offers_list.iter().any(|o| matches!(o, PyRep::KeyVal(kv) if kv.get("offerID").and_then(|v| v.as_i64()) == Some(1002))));

    let lps_rep = rpc(&ctx, &mut s1, "LPSvc", "GetAllMyCharacterWalletLPBalances", &[])
        .await.expect("GetAllMyCharacterWalletLPBalances");
    let PyRep::List(lps_list) = lps_rep else { panic!("Expected List"); };
    let cn_lp = lps_list.iter().find_map(|row| match row {
        PyRep::KeyVal(kv) if kv.get("corpID").and_then(|v| v.as_i64()) == Some(caldari_navy_corp_id) => {
            kv.get("loyaltyPoints").and_then(|v| v.as_i64())
        }
        _ => None,
    });
    assert_eq!(cn_lp, Some(5_000));

    let s1_bal_before = ctx.db.get_character(90_000_001).unwrap().balance;
    let take_res = rpc(&ctx, &mut s1, "LPStoreMgr", "TakeOfferForCharacter", &[
        PyRep::Integer(caldari_navy_corp_id), PyRep::Integer(1002), PyRep::Integer(1),
    ]).await.expect("TakeOfferForCharacter");

    let PyRep::KeyVal(take_kv) = take_res else { panic!("Expected KeyVal"); };
    assert_eq!(take_kv.get("remainingLP"), Some(&PyRep::Integer(4_500)));
    let delivered_missiles_id = take_kv.get("deliveredItemID").and_then(|v| v.as_i64()).unwrap();

    let s1_bal_after = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(s1_bal_before.0 - s1_bal_after.0, 50_000_000);

    let missile_item = ctx.db.with_snapshot(|snap| snap.items.get(&delivered_missiles_id).cloned()).unwrap();
    assert_eq!(missile_item.type_id, 27361);
    assert_eq!(missile_item.quantity, 1_000);
    assert_eq!(missile_item.location_id, JITA_STATION_ID);
    assert_eq!(missile_item.flag, InventoryFlag::Hangar);

    let has_lp_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal.iter().any(|j| j.character_id == 90_000_001 && j.entry_type == "LPStorePurchase")
    });
    assert!(has_lp_journal);

    let insufficient_lp_err = rpc(&ctx, &mut s1, "LPStoreMgr", "TakeOfferForCharacter", &[
        PyRep::Integer(caldari_navy_corp_id), PyRep::Integer(1003), PyRep::Integer(1),
    ]).await.unwrap_err();
    assert_eq!(insufficient_lp_err.code, "InsufficientLoyaltyPoints");
}
