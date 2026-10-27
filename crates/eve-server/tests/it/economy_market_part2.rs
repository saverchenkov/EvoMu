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
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord, TypeId};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
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
async fn test_market_instant_matching() {
    // parity: feat:market.instant-buy, feat:market.instant-sell
    eve_testkit::attest(&["feat:market.instant-buy", "feat:market.instant-sell"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
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

    let s2_bal_before_mb = ctx.db.get_character(90_000_002).unwrap().balance;
    let other_jita_st = ctx.sde.solar_systems.get(&30_000_142).unwrap().stations.iter().find(|s| s.station_id != JITA_STATION_ID).unwrap().station_id;
    // Seed an out-of-range sell order at other_jita_st (5.0 ISK) while BuyMultipleItems uses orderRange = -1 (station only)
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let oid = snap.next_entity_id;
        snap.market_orders.insert(
            oid,
            MarketOrderRecord {
                order_id: oid, owner_id: 90_000_002, station_id: other_jita_st, region_id: 10_000_002,
                type_id: TRITANIUM_TYPE_ID, is_bid: false, price: IskCents::from_isk(5),
                vol_entered: 200, vol_remaining: 200, min_volume: 1, is_npc: false, escrow: IskCents(0),
            },
        );
    });
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
    assert_eq!(mb_kv.get("salesTaxISK"), Some(&PyRep::Real(645.0))); // 7.5% of 8,600 ISK
    let PyRep::List(created) = mb_kv.get("ordersCreated").expect("ordersCreated") else { panic!("Expected List") };
    assert_eq!(created.len(), 2);
    let s2_bal_after_mb = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(s2_bal_after_mb.0 - s2_bal_before_mb.0, IskCents::from_f64_isk(8_600.0 - 645.0).0);
    let (has_buyer_j, has_seller_j, has_tax_j) = ctx.db.with_snapshot(|snap| {
        (
            snap.wallet_journal.iter().any(|e| e.character_id == 90_000_001 && e.entry_type == "MarketTransaction"),
            snap.wallet_journal.iter().any(|e| e.character_id == 90_000_002 && e.entry_type == "MarketSell"),
            snap.wallet_journal.iter().any(|e| e.character_id == 90_000_002 && e.entry_type == "TransactionTax"),
        )
    });
    assert!(has_buyer_j && has_seller_j && has_tax_j, "Both buyer and seller + sales tax must be journaled on BuyMultipleItems");

    // BuyMultipleItems with orderRange = 0 (solar system) fills the 5.0 ISK order at other_jita_st and delivers to other_jita_st
    let mb_sys = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "BuyMultipleItems",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                ("quantity", PyRep::Integer(100)),
                ("price", PyRep::Real(6.0)),
                ("orderRange", PyRep::Integer(0)),
            ])]),
        ],
    ).await.expect("BuyMultipleItems solar system range");
    assert_eq!(mb_sys.dict_get("totalCostISK").and_then(|v| v.as_f64()), Some(500.0));
    let delivered_at_other_st = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|i| i.owner_id == 90_000_001 && i.location_id == other_jita_st && i.type_id == TRITANIUM_TYPE_ID && i.quantity == 100)
    });
    assert!(delivered_at_other_st, "BuyMultipleItems must deliver items to the filled sell order's station");

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
            PyRep::Integer(-1), // station-only range
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

    // Selling from another station (other_jita_st) into a station-only (-1) buy order at JITA_STATION_ID is rejected by range
    let s2_other_trit = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.station_id = Some(other_jita_st);
        }
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(id, InventoryItemRecord {
            item_id: id, type_id: TRITANIUM_TYPE_ID, owner_id: 90_000_002, location_id: other_jita_st,
            flag: InventoryFlag::Hangar, quantity: 400, singleton: false, version: 1, ..Default::default()
        });
        id
    });
    s2.station_id = Some(other_jita_st);
    let out_of_range_sell = rpc(
        &ctx, &mut s2, "marketProxy", "PlaceMultiSellOrder",
        &[PyRep::List(vec![PyRep::keyval([("stationID", PyRep::Integer(other_jita_st)), ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)), ("itemID", PyRep::Integer(s2_other_trit)), ("price", PyRep::Real(1.0)), ("quantity", PyRep::Integer(400))])]), PyRep::Bool(false), PyRep::Integer(0), PyRep::Real(0.0)],
    ).await;
    assert_eq!(out_of_range_sell.unwrap_err().code, "ImmediateOrderUnfilled");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.station_id = Some(JITA_STATION_ID);
        }
    });
    s2.station_id = Some(JITA_STATION_ID);

    let s1_trit_before_isell = s1_hangar_trit;
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
    assert_eq!(s2_bal_after_isell.0 - s2_bal_before_isell.0, IskCents::from_f64_isk(400.0 - 30.0).0);
    let s1_trit_after_isell = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.owner_id == 90_000_001 && i.location_id == JITA_STATION_ID && i.flag == InventoryFlag::Hangar && i.type_id == TRITANIUM_TYPE_ID).map(|i| i.quantity).sum::<i32>()
    });
    assert_eq!(s1_trit_after_isell - s1_trit_before_isell, 400);
}
