//! Phase 7 Economy & Market Lifecycle Integration Tests (E-1..E-6).
//!
//! Covers:
//! - E-1: Broker fee calculation with Broker Relations skill discount & Sales tax calculation with Accounting skill discount
//! - E-2: Market order matching engine: limit order placement, regional order book queries, price history, immediate fill matching, and cancellation with escrow/item refunds
//! - E-3: Contracts system: ItemExchange, Courier with collateral hold & completion, and Auctions with bidding, outbid refund & buyout
//! - E-4: Industry jobs engine: manufacturing, TE/ME research, copying, BOM material consumption, facility tax deduction, and job completion
//! - E-5: Planetary Interaction: colony extraction cycles, command center upgrades requiring skill 2505, processor conversion, and storage routing
//! - E-6: LP store: corporate offerings lookup, LP deduction + ISK payment, delivering items to hangar, and wallet LP balance queries

#![forbid(unsafe_code)]

mod common;

use eve_db::{
    AccountRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord,
    TypeId, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_PLANET_ID: i64 = 40_009_082;
const TRITANIUM_TYPE_ID: TypeId = 34;
const RIFTER_BP_ITEM_ID: i64 = 100_000_007;
const RIFTER_BP_TYPE_ID: TypeId = 691;
const BROKER_RELATIONS_SKILL: TypeId = 3446;
const ACCOUNTING_SKILL: TypeId = 16622;
const COMMAND_CENTER_UPGRADES_SKILL: TypeId = 2505;

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
async fn test_e1_broker_fee_and_sales_tax_skill_discounts() {
    // parity: plan:E-1
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut s1 = setup_pilot(&ctx, 101, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 102, "other", "other-secret", 90_000_002).await;

    // 1. Broker fee verification at level 0 vs level 5
    // Default formula: rate = 0.03 - 0.003 * broker_relations_lvl - 0.0003 * faction - 0.0002 * corp
    // Clear standings and skill to test pure 3.0% base rate at level 0:
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.standings.clear();
        }
        snap.skills
            .entry(90_000_001)
            .or_default()
            .insert(BROKER_RELATIONS_SKILL, TrainedSkillRecord {
                type_id: BROKER_RELATIONS_SKILL,
                level: 0,
                skill_points: 0.0,
            });
    });

    // Place buy order for 10,000 units at 1.00 ISK (gross: 10,000.00 ISK, below market asks so no immediate match)
    // Broker fee at level 0 (3.0%): 300.00 ISK
    let order_l0 = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(1.0),
                PyRep::Integer(10_000),
                PyRep::Integer(90),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect("PlaceBuyOrder level 0");

    let PyRep::KeyVal(kv_l0) = order_l0 else {
        panic!("Expected KeyVal from PlaceBuyOrder");
    };
    assert_eq!(kv_l0.get("filledImmediately"), Some(&PyRep::Bool(false)));
    let fee_l0 = kv_l0
        .get("brokerFeeISK")
        .and_then(|v| v.as_f64())
        .expect("brokerFeeISK");
    assert!(
        (fee_l0 - 300.0).abs() < 0.01,
        "Broker fee at level 0 should be 300.00 ISK, got {fee_l0}"
    );

    // Cancel order to clean up
    let oid_l0 = kv_l0.get("orderID").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(
        &mut s1,
        "marketProxy",
        None,
        "CancelCharOrder",
        &[PyRep::Integer(oid_l0)],
        &kw,
        None,
    )
    .await
    .expect("CancelCharOrder");

    // Train Broker Relations (3446) to level 5 (1.5% broker fee rate):
    // rate = 0.03 - 0.003 * 5 = 0.015 (1.5%) -> 150.00 ISK
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills
            .entry(90_000_001)
            .or_default()
            .insert(BROKER_RELATIONS_SKILL, TrainedSkillRecord {
                type_id: BROKER_RELATIONS_SKILL,
                level: 5,
                skill_points: 256_000.0,
            });
    });

    let order_l5 = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(1.0),
                PyRep::Integer(10_000),
                PyRep::Integer(90),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect("PlaceBuyOrder level 5");

    let PyRep::KeyVal(kv_l5) = order_l5 else {
        panic!("Expected KeyVal from PlaceBuyOrder");
    };
    assert_eq!(kv_l5.get("filledImmediately"), Some(&PyRep::Bool(false)));
    let fee_l5 = kv_l5
        .get("brokerFeeISK")
        .and_then(|v| v.as_f64())
        .expect("brokerFeeISK");
    assert!(
        (fee_l5 - 150.0).abs() < 0.01,
        "Broker fee at level 5 should be 150.00 ISK, got {fee_l5}"
    );

    let oid_l5 = kv_l5.get("orderID").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(
        &mut s1,
        "marketProxy",
        None,
        "CancelCharOrder",
        &[PyRep::Integer(oid_l5)],
        &kw,
        None,
    )
    .await
    .expect("CancelCharOrder");

    // Verify wallet journal recorded MarketBrokerFee entries
    let has_broker_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal
            .iter()
            .any(|j| j.character_id == 90_000_001 && j.entry_type == "MarketBrokerFee")
    });
    assert!(has_broker_journal, "Wallet journal must record MarketBrokerFee");

    // 2. Sales tax verification with Accounting skill discount:
    // Formula: compute_sales_tax_rate(lvl) = 0.075 * (1.0 - 0.11 * lvl)
    // Level 0: 7.5%
    // Level 5: 3.375%

    // Setup pilot 1 with a resting buy order at price 1.00 ISK for 200 units (below NPC asks at 5.00 ISK)
    let buy_order = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(1.0),
                PyRep::Integer(200),
                PyRep::Integer(90),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect("Place buy order");
    let PyRep::KeyVal(buy_kv) = buy_order else {
        panic!("Expected KeyVal");
    };
    assert_eq!(buy_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));

    // Give pilot 2 some Tritanium in hangar
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
        // Ensure Accounting skill is level 0
        snap.skills
            .entry(90_000_002)
            .or_default()
            .insert(ACCOUNTING_SKILL, TrainedSkillRecord {
                type_id: ACCOUNTING_SKILL,
                level: 0,
                skill_points: 0.0,
            });
    });

    let s2_bal_before = ctx.db.get_character(90_000_002).unwrap().balance;

    // Pilot 2 sells 100 units at 1.00 ISK (matches standing NPC bid at 1.88 ISK, gross = 188.00 ISK)
    // Accounting level 0: tax rate = 7.5%, sales tax = 14.10 ISK, net = 173.90 ISK
    let sell_l0 = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
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
            &kw,
            None,
        )
        .await
        .expect("PlaceMultiSellOrder sell level 0");

    let PyRep::KeyVal(sell_l0_kv) = sell_l0 else {
        panic!("Expected KeyVal");
    };
    assert_eq!(sell_l0_kv.get("filledImmediately"), Some(&PyRep::Bool(true)));
    let tax_l0 = sell_l0_kv
        .get("salesTaxISK")
        .and_then(|v| v.as_f64())
        .expect("salesTaxISK");
    assert!(
        (tax_l0 - 14.10).abs() < 0.01,
        "Sales tax at level 0 should be 14.10 ISK (7.5% of 188.00 ISK), got {tax_l0}"
    );

    let broker_l0 = sell_l0_kv
        .get("brokerFeeISK")
        .and_then(|v| v.as_f64())
        .expect("brokerFeeISK");
    let s2_bal_after = ctx.db.get_character(90_000_002).unwrap().balance;
    let net_gain = (s2_bal_after.0 - s2_bal_before.0) as f64 / 100.0;
    let expected_gain = 188.0 - 14.10 - broker_l0;
    assert!(
        (net_gain - expected_gain).abs() < 0.02,
        "Net gain {net_gain} should match expected {expected_gain}"
    );

    // Now train Pilot 2 Accounting (16622) to level 5 (2.5% sales tax rate)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills
            .entry(90_000_002)
            .or_default()
            .insert(ACCOUNTING_SKILL, TrainedSkillRecord {
                type_id: ACCOUNTING_SKILL,
                level: 5,
                skill_points: 256_000.0,
            });
    });

    // Pilot 2 sells another 100 units (matches standing NPC bid at 1.88 ISK, gross = 188.00 ISK)
    // Accounting level 5: tax rate = 3.375%, sales tax = 6.35 ISK, net = 181.65 ISK
    let sell_l5 = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
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
            &kw,
            None,
        )
        .await
        .expect("PlaceMultiSellOrder sell level 5");

    let PyRep::KeyVal(sell_l5_kv) = sell_l5 else {
        panic!("Expected KeyVal");
    };
    assert_eq!(sell_l5_kv.get("filledImmediately"), Some(&PyRep::Bool(true)));
    let tax_l5 = sell_l5_kv
        .get("salesTaxISK")
        .and_then(|v| v.as_f64())
        .expect("salesTaxISK");
    assert!(
        (tax_l5 - 6.35).abs() < 0.01,
        "Sales tax at level 5 should be 6.35 ISK (3.375% of 188.00 ISK), got {tax_l5}"
    );
}

#[tokio::test]
async fn test_e2_market_order_matching_queries_and_cancellation() {
    // parity: plan:E-2
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut s1 = setup_pilot(&ctx, 201, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 202, "other", "other-secret", 90_000_002).await;

    // 1. Order book & regional queries
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
    assert!(matches!(orders_rep, PyRep::Tuple(_)));

    let asks_rep = ctx
        .dispatch_rpc(&mut s1, "marketProxy", None, "GetStationAsks", &[], &kw, None)
        .await
        .expect("GetStationAsks");
    assert!(asks_rep.is_cindexed_rowset(), "GetStationAsks must return cindexed rowset");

    let region_rep = ctx
        .dispatch_rpc(&mut s1, "marketProxy", None, "GetRegionBest", &[], &kw, None)
        .await
        .expect("GetRegionBest");
    assert!(region_rep.is_cindexed_rowset(), "GetRegionBest must return cindexed rowset");

    let hist_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "GetOldPriceHistory",
            &[PyRep::Integer(TRITANIUM_TYPE_ID as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetOldPriceHistory");
    assert!(hist_rep.is_crowset(), "GetOldPriceHistory must return CRowset");

    let new_hist_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "GetNewPriceHistory",
            &[PyRep::Integer(TRITANIUM_TYPE_ID as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetNewPriceHistory");
    assert!(new_hist_rep.is_crowset(), "GetNewPriceHistory must return CRowset");

    // 2. Limit sell order placement & cancellation (item refund to station hangar)
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
    assert!(s1_trit_before >= 1_000, "Pilot 1 must have at least 1,000 Tritanium in hangar");

    // Place sell order for 1,000 Tritanium at 15.00 ISK
    let sell_order = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
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
            &kw,
            None,
        )
        .await
        .expect("Place sell order");
    let PyRep::KeyVal(sell_kv) = sell_order else {
        panic!("Expected KeyVal");
    };
    assert_eq!(sell_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));
    let sell_oid = sell_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();

    // Verify Tritanium consumed from hangar
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
    assert_eq!(
        s1_trit_after_place,
        s1_trit_before - 1_000,
        "Placing sell order must deduct items from hangar"
    );

    // Verify order in GetCharOrders
    let char_orders = ctx
        .dispatch_rpc(&mut s1, "marketProxy", None, "GetCharOrders", &[], &kw, None)
        .await
        .expect("GetCharOrders");
    let PyRep::Rowset { rows: co_rows, .. } = char_orders else {
        panic!("Expected Rowset for GetCharOrders");
    };
    let order_present = co_rows
        .iter()
        .any(|row| row.first().and_then(|v| v.as_i64()) == Some(sell_oid));
    assert!(order_present, "Order must appear in GetCharOrders");

    // Cancel sell order: items must return to station hangar
    ctx.dispatch_rpc(
        &mut s1,
        "marketProxy",
        None,
        "CancelCharOrder",
        &[PyRep::Integer(sell_oid)],
        &kw,
        None,
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
    assert_eq!(
        s1_trit_after_cancel, s1_trit_before,
        "Cancelling sell order must restore items back to station hangar"
    );

    // 3. Immediate fill order matching
    // Pilot 1 places a resting sell order for 100 units at 3.00 ISK (cheaper than NPC asks at 5.00 ISK)
    let resting_sell = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
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
            &kw,
            None,
        )
        .await
        .expect("Place resting sell order");
    let PyRep::KeyVal(r_sell_kv) = resting_sell else {
        panic!("Expected KeyVal");
    };
    let resting_oid = r_sell_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();

    // Pilot 2 places a matching buy order for 40 units at 3.00 ISK
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

    let match_buy = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(3.00),
                PyRep::Integer(40),
                PyRep::Integer(90),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect("Place matching buy order");
    let PyRep::KeyVal(match_kv) = match_buy else {
        panic!("Expected KeyVal");
    };
    assert_eq!(
        match_kv.get("filledImmediately"),
        Some(&PyRep::Bool(true)),
        "Order matching against resting asks must fill immediately"
    );

    // Verify Pilot 2 received 40 Tritanium in hangar
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

    // Verify resting order volume decremented to 60
    let rem_vol = ctx
        .db
        .with_snapshot(|snap| snap.market_orders.get(&resting_oid).map(|o| o.vol_remaining))
        .unwrap();
    assert_eq!(rem_vol, 60, "Resting order volume must be reduced by fill quantity");

    // Clean up resting sell
    ctx.dispatch_rpc(
        &mut s1,
        "marketProxy",
        None,
        "CancelCharOrder",
        &[PyRep::Integer(resting_oid)],
        &kw,
        None,
    )
    .await
    .expect("CancelCharOrder");

    // 4. Limit buy order placement with escrow & cancellation (escrow refund)
    let s2_bal_before_buy = ctx.db.get_character(90_000_002).unwrap().balance;
    let limit_buy = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(2.00),
                PyRep::Integer(500), // gross = 1,000.00 ISK
                PyRep::Integer(90),
                PyRep::Integer(1),
            ],
            &kw,
            None,
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
    assert!(
        (deducted - (1000.0 + l_buy_fee)).abs() < 0.02,
        "Buy order placement must deduct escrow (1,000 ISK) + broker fee ({l_buy_fee})"
    );

    // Cancel buy order: 1,000.00 ISK escrow must be refunded back to character balance
    ctx.dispatch_rpc(
        &mut s2,
        "marketProxy",
        None,
        "CancelCharOrder",
        &[PyRep::Integer(l_buy_oid)],
        &kw,
        None,
    )
    .await
    .expect("Cancel buy order");

    let s2_bal_after_cancel = ctx.db.get_character(90_000_002).unwrap().balance;
    let refunded = (s2_bal_after_cancel.0 - s2_bal_after_buy.0) as f64 / 100.0;
    assert!(
        (refunded - 1000.0).abs() < 0.01,
        "Escrow of 1,000.00 ISK must be refunded upon cancellation, got {refunded}"
    );

    let has_cancel_refund = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal.iter().any(|j| {
            j.character_id == 90_000_002 && j.entry_type == "MarketOrderCancelRefund"
        })
    });
    assert!(has_cancel_refund, "Wallet journal must record MarketOrderCancelRefund");
}

#[tokio::test]
async fn test_e3_contracts_lifecycle_item_exchange_courier_auction() {
    // parity: plan:E-3
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut s1 = setup_pilot(&ctx, 301, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 302, "other", "other-secret", 90_000_002).await;

    // Add 3rd pilot for auction outbidding
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "third".to_string(),
            AccountRecord {
                user_id: 3,
                username: "third".to_string(),
                password_token: WorldRepository::hash_password_token("third", "third-secret"),
                role: 1,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut c3 = snap.characters[&90_000_002].clone();
        c3.character_id = 90_000_003;
        c3.user_id = 3;
        c3.name = "Third Pilot".to_string();
        c3.balance = IskCents::from_isk(50_000_000);
        c3.station_id = Some(JITA_STATION_ID);
        snap.characters.insert(90_000_003, c3);
    });
    let mut s3 = setup_pilot(&ctx, 303, "third", "third-secret", 90_000_003).await;

    // 1. ItemExchange Contract
    // Character 1 creates contract selling 1MN Afterburner II (100_000_003) for 50,000 ISK
    let c_exchange = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(1), // ItemExchange
                PyRep::Integer(0), // Public
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Real(50_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
                PyRep::String("".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(100_000_003)]),
            ],
            &kw,
            None,
        )
        .await
        .expect("Create ItemExchange Contract");
    let PyRep::KeyVal(c_ex_kv) = c_exchange else {
        panic!("Expected KeyVal");
    };
    let ex_cid = c_ex_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();

    // Verify item is locked in contract escrow
    let item_locked = ctx.db.with_snapshot(|snap| {
        snap.items.get(&100_000_003).map(|i| {
            i.flag == InventoryFlag::Locked && i.location_id == ex_cid
        })
    });
    assert_eq!(item_locked, Some(true), "Contract item must be locked with contract as location");

    // SearchContracts finds the contract
    let search_rep = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "SearchContracts",
            &[PyRep::String("ItemExchange".into())],
            &kw,
            None,
        )
        .await
        .expect("SearchContracts");
    let PyRep::List(contracts_list) = search_rep else {
        panic!("Expected List from SearchContracts");
    };
    assert!(contracts_list.iter().any(|c| match c {
        PyRep::KeyVal(kv) => kv.get("contractID").and_then(|v| v.as_i64()) == Some(ex_cid),
        _ => false,
    }));

    // Character 1 queries GetMyCurrentContractList
    let my_contracts = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "GetMyCurrentContractList",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetMyCurrentContractList");
    let PyRep::List(my_list) = my_contracts else {
        panic!("Expected List");
    };
    assert!(my_list.iter().any(|c| match c {
        PyRep::KeyVal(kv) => kv.get("contractID").and_then(|v| v.as_i64()) == Some(ex_cid),
        _ => false,
    }));

    // Character 2 accepts the contract
    let s1_bal_before = ctx.db.get_character(90_000_001).unwrap().balance;
    let s2_bal_before = ctx.db.get_character(90_000_002).unwrap().balance;

    let accept_rep = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "AcceptContract",
            &[PyRep::Integer(ex_cid)],
            &kw,
            None,
        )
        .await
        .expect("Accept ItemExchange Contract");
    let PyRep::KeyVal(accept_kv) = accept_rep else {
        panic!("Expected KeyVal");
    };
    assert_eq!(accept_kv.get("status"), Some(&PyRep::String("Completed".into())));

    // Check ISK balances: 50,000 ISK transferred
    let s1_bal_after = ctx.db.get_character(90_000_001).unwrap().balance;
    let s2_bal_after = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(s1_bal_after.0 - s1_bal_before.0, 5_000_000); // 50,000.00 ISK in cents
    assert_eq!(s2_bal_before.0 - s2_bal_after.0, 5_000_000);

    // Check item ownership transferred to Character 2 in station hangar
    let item_transferred = ctx.db.with_snapshot(|snap| {
        snap.items.get(&100_000_003).map(|i| {
            i.owner_id == 90_000_002
                && i.location_id == JITA_STATION_ID
                && i.flag == InventoryFlag::Hangar
        })
    });
    assert_eq!(item_transferred, Some(true), "Item must be transferred to buyer's hangar");

    // 2. Courier Contract
    // Create an item for Character 1 to courier
    let courier_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: TRITANIUM_TYPE_ID,
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        id
    });

    let c_courier = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(3), // Courier
                PyRep::Integer(0), // Public
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Real(0.0),
                PyRep::Real(20_000.0),  // reward
                PyRep::Real(100_000.0), // collateral
                PyRep::String("".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(courier_item_id)]),
            ],
            &kw,
            None,
        )
        .await
        .expect("Create Courier Contract");
    let PyRep::KeyVal(c_courier_kv) = c_courier else {
        panic!("Expected KeyVal");
    };
    let courier_cid = c_courier_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();

    // Character 2 accepts the courier contract: collateral 100,000 ISK deducted
    let s2_bal_pre_courier = ctx.db.get_character(90_000_002).unwrap().balance;
    let accept_courier = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "AcceptContract",
            &[PyRep::Integer(courier_cid)],
            &kw,
            None,
        )
        .await
        .expect("Accept Courier Contract");
    let PyRep::KeyVal(ac_kv) = accept_courier else {
        panic!("Expected KeyVal");
    };
    assert_eq!(ac_kv.get("status"), Some(&PyRep::String("InProgress".into())));

    let s2_bal_post_accept = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(s2_bal_pre_courier.0 - s2_bal_post_accept.0, 10_000_000); // 100,000 ISK collateral in cents

    // Character 2 completes the courier contract: 100,000 ISK collateral refunded + 20,000 ISK reward paid
    let complete_courier = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "CompleteContract",
            &[PyRep::Integer(courier_cid)],
            &kw,
            None,
        )
        .await
        .expect("Complete Courier Contract");
    let PyRep::KeyVal(cc_kv) = complete_courier else {
        panic!("Expected KeyVal");
    };
    assert_eq!(cc_kv.get("status"), Some(&PyRep::String("Completed".into())));
    let payout = cc_kv.get("totalPayoutISK").and_then(|v| v.as_f64()).unwrap();
    assert_eq!(payout, 120_000.0, "Payout must include collateral (100k) + reward (20k)");

    let s2_bal_post_complete = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(s2_bal_post_complete.0 - s2_bal_post_accept.0, 12_000_000); // +120,000 ISK in cents

    // 3. Auction Contract
    // Create an item to auction
    let auction_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 587, // Rifter
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        id
    });

    let c_auction = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(2), // Auction
                PyRep::Integer(0), // Public
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Real(10_000.0), // starting bid
                PyRep::Real(0.0),
                PyRep::Real(50_000.0), // buyout
                PyRep::String("".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(auction_item_id)]),
            ],
            &kw,
            None,
        )
        .await
        .expect("Create Auction Contract");
    let PyRep::KeyVal(c_auc_kv) = c_auction else {
        panic!("Expected KeyVal");
    };
    let auc_cid = c_auc_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();

    // Pilot 2 places bid of 15,000 ISK
    let s2_bal_before_bid = ctx.db.get_character(90_000_002).unwrap().balance;
    let bid_1 = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "PlaceBid",
            &[PyRep::Integer(auc_cid), PyRep::Real(15_000.0)],
            &kw,
            None,
        )
        .await
        .expect("PlaceBid 1");
    let PyRep::KeyVal(b1_kv) = bid_1 else {
        panic!("Expected KeyVal");
    };
    assert_eq!(b1_kv.get("boughtOut"), Some(&PyRep::Bool(false)));
    let s2_bal_after_bid = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(s2_bal_before_bid.0 - s2_bal_after_bid.0, 1_500_000); // 15,000 ISK deducted

    // Pilot 3 outbids Pilot 2 with 25,000 ISK
    let s3_bal_before_bid = ctx.db.get_character(90_000_003).unwrap().balance;
    let bid_2 = ctx
        .dispatch_rpc(
            &mut s3,
            "contractProxy",
            None,
            "PlaceBid",
            &[PyRep::Integer(auc_cid), PyRep::Real(25_000.0)],
            &kw,
            None,
        )
        .await
        .expect("PlaceBid 2");
    let PyRep::KeyVal(b2_kv) = bid_2 else {
        panic!("Expected KeyVal");
    };
    assert_eq!(b2_kv.get("boughtOut"), Some(&PyRep::Bool(false)));

    // Verify Pilot 2 was refunded 15,000 ISK immediately upon being outbid
    let s2_bal_after_outbid = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(
        s2_bal_after_outbid, s2_bal_before_bid,
        "Pilot 2 must receive full refund of 15,000 ISK after being outbid"
    );

    let has_outbid_refund = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal.iter().any(|j| {
            j.character_id == 90_000_002 && j.entry_type == "ContractAuctionBidRefund"
        })
    });
    assert!(has_outbid_refund, "Wallet journal must record ContractAuctionBidRefund");

    // Pilot 2 meets buyout price of 50,000 ISK
    let bid_buyout = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "PlaceBid",
            &[PyRep::Integer(auc_cid), PyRep::Real(50_000.0)],
            &kw,
            None,
        )
        .await
        .expect("PlaceBid Buyout");
    let PyRep::KeyVal(bb_kv) = bid_buyout else {
        panic!("Expected KeyVal");
    };
    assert_eq!(bb_kv.get("boughtOut"), Some(&PyRep::Bool(true)));
    assert_eq!(bb_kv.get("status"), Some(&PyRep::String("Completed".into())));

    // Pilot 3 must be refunded their 25,000 ISK bid
    let s3_bal_after_buyout = ctx.db.get_character(90_000_003).unwrap().balance;
    assert_eq!(
        s3_bal_after_buyout, s3_bal_before_bid,
        "Pilot 3 must receive full refund of 25,000 ISK after auction buyout"
    );

    // Pilot 2 receives auction item in hangar
    let item_in_s2_hangar = ctx.db.with_snapshot(|snap| {
        snap.items.get(&auction_item_id).map(|i| {
            i.owner_id == 90_000_002
                && i.location_id == JITA_STATION_ID
                && i.flag == InventoryFlag::Hangar
        })
    });
    assert_eq!(item_in_s2_hangar, Some(true), "Auction winner must receive item in hangar");
}

#[tokio::test]
async fn test_e4_industry_jobs_lifecycle_manufacturing_research_and_copying() {
    // parity: plan:E-4
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut s1 = setup_pilot(&ctx, 401, "demo", "demo-secret", 90_000_001).await;

    // 1. Manufacturing Job (activity_id = 1)
    // Pilot 1 has Rifter Blueprint (RIFTER_BP_ITEM_ID, BPO, type 691) and materials in Jita
    let trit_before = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| {
                i.owner_id == 90_000_001
                    && i.location_id == JITA_STATION_ID
                    && i.flag == InventoryFlag::Hangar
                    && i.type_id == TRITANIUM_TYPE_ID
            })
            .map(|i| i.quantity)
            .unwrap_or(0)
    });
    assert!(trit_before >= 32_000, "Ensure pilot has enough Tritanium for manufacturing");

    let s1_bal_before_job = ctx.db.get_character(90_000_001).unwrap().balance;

    // Install Manufacturing job: 1 run
    let mfg_install = ctx
        .dispatch_rpc(
            &mut s1,
            "industryManager",
            None,
            "InstallJob",
        &[
            PyRep::Integer(RIFTER_BP_TYPE_ID as i64),
            PyRep::Integer(1), // activityID = Manufacturing
            PyRep::Integer(1), // runs
        ],
        &kw,
        None,
    )
    .await
    .expect("InstallJob Manufacturing");

    let PyRep::KeyVal(mfg_kv) = mfg_install else {
        panic!("Expected KeyVal");
    };
    let mfg_job_id = mfg_kv.get("jobID").and_then(|v| v.as_i64()).unwrap();
    let tax_charged = mfg_kv.get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert_eq!(tax_charged, 1_000.0, "Facility tax for 1 run at 10% base cost 10,000 ISK must be 1,000.0 ISK");

    // Verify facility tax was deducted from character balance and recorded in wallet journal
    let s1_bal_after_install = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(s1_bal_before_job.0 - s1_bal_after_install.0, 100_000); // 1,000.00 ISK in cents
    let has_tax_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal.iter().any(|j| {
            j.character_id == 90_000_001 && j.entry_type == "IndustryFacilityTax"
        })
    });
    assert!(has_tax_journal, "Wallet journal must record IndustryFacilityTax");

    // Verify blueprint is locked during the job
    let bp_flag = ctx.db.with_snapshot(|snap| {
        snap.items.get(&RIFTER_BP_ITEM_ID).map(|i| i.flag)
    });
    assert_eq!(bp_flag, Some(InventoryFlag::Locked), "Blueprint must be locked while job is active");

    // Query GetJobsByOwner
    let jobs_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "industryManager",
            None,
            "GetJobsByOwner",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetJobsByOwner");
    let PyRep::List(jobs_list) = jobs_rep else {
        panic!("Expected List");
    };
    let job_entry = jobs_list.iter().find(|j| match j {
        PyRep::KeyVal(kv) => kv.get("jobID").and_then(|v| v.as_i64()) == Some(mfg_job_id),
        _ => false,
    });
    assert!(job_entry.is_some(), "Active job must appear in GetJobsByOwner");

    // Fast-forward start_unix into past so job duration has elapsed
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(j) = snap.industry_jobs.get_mut(&mfg_job_id) {
            j.start_unix = 0;
        }
    });

    // CompleteJob: delivers manufactured ship and unlocks blueprint
    let complete_mfg = ctx
        .dispatch_rpc(
            &mut s1,
            "industryManager",
            None,
            "CompleteJob",
            &[PyRep::Integer(mfg_job_id)],
            &kw,
            None,
        )
        .await
        .expect("CompleteJob Manufacturing");
    let PyRep::KeyVal(cmfg_kv) = complete_mfg else {
        panic!("Expected KeyVal");
    };
    let delivered_ship_id = cmfg_kv.get("deliveredItemID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(cmfg_kv.get("productTypeID"), Some(&PyRep::Integer(587))); // Rifter

    let ship_in_hangar = ctx.db.with_snapshot(|snap| {
        snap.items.get(&delivered_ship_id).map(|i| {
            i.owner_id == 90_000_001
                && i.location_id == JITA_STATION_ID
                && i.flag == InventoryFlag::Hangar
                && i.type_id == 587
        })
    });
    assert_eq!(ship_in_hangar, Some(true), "Manufactured ship must be delivered to station hangar");

    // Blueprint must be unlocked back to Hangar
    let bp_unlocked = ctx.db.with_snapshot(|snap| {
        snap.items.get(&RIFTER_BP_ITEM_ID).map(|i| i.flag)
    });
    assert_eq!(bp_unlocked, Some(InventoryFlag::Hangar));

    // 2. Research TE (activity 3)
    let te_install = ctx
        .dispatch_rpc(
            &mut s1,
            "industryManager",
            None,
            "InstallJob",
            &[
                PyRep::Integer(RIFTER_BP_TYPE_ID as i64),
                PyRep::Integer(3), // activityID = Research TE
                PyRep::Integer(1), // runs
            ],
            &kw,
            None,
        )
        .await
        .expect("InstallJob TE");
    let PyRep::KeyVal(te_kv) = te_install else {
        panic!("Expected KeyVal");
    };
    let te_job_id = te_kv.get("jobID").and_then(|v| v.as_i64()).unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(j) = snap.industry_jobs.get_mut(&te_job_id) {
            j.start_unix = 0;
        }
    });

    ctx.dispatch_rpc(
        &mut s1,
        "industryManager",
        None,
        "CompleteJob",
        &[PyRep::Integer(te_job_id)],
        &kw,
        None,
    )
    .await
    .expect("CompleteJob TE");

    let te_level = ctx.db.with_snapshot(|snap| {
        snap.items
            .get(&RIFTER_BP_ITEM_ID)
            .and_then(|bp| bp.dynamic_attributes.get(&1113).copied())
    });
    assert_eq!(te_level, Some(2.0), "Research TE of 1 run must increase TE attribute to 2.0");

    // 3. Research ME (activity 4)
    let me_install = ctx
        .dispatch_rpc(
            &mut s1,
            "industryManager",
            None,
            "InstallJob",
            &[
                PyRep::Integer(RIFTER_BP_TYPE_ID as i64),
                PyRep::Integer(4), // activityID = Research ME
                PyRep::Integer(1), // runs
            ],
            &kw,
            None,
        )
        .await
        .expect("InstallJob ME");
    let PyRep::KeyVal(me_kv) = me_install else {
        panic!("Expected KeyVal");
    };
    let me_job_id = me_kv.get("jobID").and_then(|v| v.as_i64()).unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(j) = snap.industry_jobs.get_mut(&me_job_id) {
            j.start_unix = 0;
        }
    });

    ctx.dispatch_rpc(
        &mut s1,
        "industryManager",
        None,
        "CompleteJob",
        &[PyRep::Integer(me_job_id)],
        &kw,
        None,
    )
    .await
    .expect("CompleteJob ME");

    let me_level = ctx.db.with_snapshot(|snap| {
        snap.items
            .get(&RIFTER_BP_ITEM_ID)
            .and_then(|bp| bp.dynamic_attributes.get(&1112).copied())
    });
    assert_eq!(me_level, Some(1.0), "Research ME of 1 run must increase ME attribute to 1.0");

    // 4. Copying (activity 5)
    let copy_install = ctx
        .dispatch_rpc(
            &mut s1,
            "industryManager",
            None,
            "InstallJob",
            &[
                PyRep::Integer(RIFTER_BP_TYPE_ID as i64),
                PyRep::Integer(5), // activityID = Copying
                PyRep::Integer(3), // runs
            ],
            &kw,
            None,
        )
        .await
        .expect("InstallJob Copying");
    let PyRep::KeyVal(copy_kv) = copy_install else {
        panic!("Expected KeyVal");
    };
    let copy_job_id = copy_kv.get("jobID").and_then(|v| v.as_i64()).unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(j) = snap.industry_jobs.get_mut(&copy_job_id) {
            j.start_unix = 0;
        }
    });

    let complete_copy = ctx
        .dispatch_rpc(
            &mut s1,
            "industryManager",
            None,
            "CompleteJob",
            &[PyRep::Integer(copy_job_id)],
            &kw,
            None,
        )
        .await
        .expect("CompleteJob Copying");
    let PyRep::KeyVal(ccopy_kv) = complete_copy else {
        panic!("Expected KeyVal");
    };
    let bpc_item_id = ccopy_kv.get("deliveredItemID").and_then(|v| v.as_i64()).unwrap();

    // Verify delivered item is a BPC with runs = 3
    let bpc_record = ctx.db.with_snapshot(|snap| snap.items.get(&bpc_item_id).cloned()).unwrap();
    assert!(bpc_record.is_blueprint_copy, "Output must be a blueprint copy");
    assert_eq!(bpc_record.blueprint_runs_remaining, Some(3), "BPC must have 3 runs");
    assert_eq!(bpc_record.type_id, RIFTER_BP_TYPE_ID);
}

#[tokio::test]
async fn test_e5_planetary_interaction_lifecycle() {
    // parity: plan:E-5
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut s1 = setup_pilot(&ctx, 501, "demo", "demo-secret", 90_000_001).await;

    // 1. Create Colony on Jita planet 40009082 using COMMAND_CREATEPIN = 1 while undocked with CC in ship cargo
    let s1_ship_id = s1.ship_id.expect("s1 active ship");
    s1.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
        }
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 2254,
                owner_id: 90_000_001,
                location_id: s1_ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let create_colony = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Integer(1),
                    PyRep::Integer(2254),
                    PyRep::Real(0.1),
                    PyRep::Real(0.2),
                ])]),
            ],
            &kw,
            None,
        )
        .await
        .expect("UserUpdateNetwork");
    s1.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(JITA_STATION_ID);
        }
    });
    let PyRep::KeyVal(col_kv) = create_colony else {
        panic!("Expected KeyVal");
    };
    assert_eq!(col_kv.get("commandCenterLevel"), Some(&PyRep::Integer(0)));
    assert_eq!(col_kv.get("pinsCount"), Some(&PyRep::Integer(1)));

    // 2. Command Center Upgrades validation
    // Clear skill 2505 to verify missing skill error
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills
            .entry(90_000_001)
            .or_default()
            .remove(&COMMAND_CENTER_UPGRADES_SKILL);
    });

    let upgrade_err = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "UpgradeCommandCenter",
            &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        upgrade_err.code, "MissingSkillForCommandCenterUpgrade",
        "Upgrading CC to level 2 without trained skill 2505 must be rejected"
    );

    // Train Command Center Upgrades (2505) to level 3
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills
            .entry(90_000_001)
            .or_default()
            .insert(COMMAND_CENTER_UPGRADES_SKILL, TrainedSkillRecord {
                type_id: COMMAND_CENTER_UPGRADES_SKILL,
                level: 3,
                skill_points: 256_000.0,
            });
    });

    let s1_bal_before_up = ctx.db.get_character(90_000_001).unwrap().balance;

    // Upgrade Command Center from level 0 to level 2 (cost: 580,000 + 930,000 = 1,510,000 ISK)
    let upgrade_res = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "UpgradeCommandCenter",
            &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("UpgradeCommandCenter level 2");
    let PyRep::KeyVal(up_kv) = upgrade_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(up_kv.get("commandCenterLevel"), Some(&PyRep::Integer(2)));
    assert_eq!(up_kv.get("pinsCount"), Some(&PyRep::Integer(3)));

    let s1_bal_after_up = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(s1_bal_before_up.0 - s1_bal_after_up.0, 151_000_000); // 1,510,000 ISK in cents

    let has_cc_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal.iter().any(|j| {
            j.character_id == 90_000_001 && j.entry_type == "PlanetCommandCenterUpgrade"
        })
    });
    assert!(has_cc_journal, "Wallet journal must record PlanetCommandCenterUpgrade");

    // 3. Processor Conversion: UserTransferCommodities
    // Seed 6,000 units of P0 commodity (type 2288: Aqueous Liquids) into colony storage
    let p0_type: TypeId = 2288;
    let p1_type: TypeId = 3645; // Water
    ctx.db.with_mut_snapshot(|snap| {
        let col = snap.planetary_colonies.get_mut(&(90_000_001, JITA_PLANET_ID)).unwrap();
        col.extracted_commodities.insert(p0_type, 6_000);
    });

    // Run 2 processor conversion cycles (3,000 P0 -> 20 P1 per cycle)
    let process_res = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "UserTransferCommodities",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::Integer(p0_type as i64),
                PyRep::Integer(p1_type as i64),
                PyRep::Integer(2), // 2 cycles
            ],
            &kw,
            None,
        )
        .await
        .expect("UserTransferCommodities");
    let PyRep::KeyVal(proc_kv) = process_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(proc_kv.get("inputQuantityConsumed"), Some(&PyRep::Integer(6_000)));
    assert_eq!(proc_kv.get("outputQuantityProduced"), Some(&PyRep::Integer(40)));
    assert_eq!(proc_kv.get("totalStoredOutput"), Some(&PyRep::Integer(40)));

    // 4. Storage Routing: UserLaunchCommodities to Hangar
    let route_res = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "UserLaunchCommodities",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::Integer(p1_type as i64),
                PyRep::Integer(40),
                PyRep::String("Hangar".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("UserLaunchCommodities");
    let PyRep::KeyVal(route_kv) = route_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(route_kv.get("routedQuantity"), Some(&PyRep::Integer(40)));

    // Verify 40 units of Water (p1_type) materialized in character's Jita station hangar
    let hangar_water = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                i.owner_id == 90_000_001
                    && i.location_id == JITA_STATION_ID
                    && i.flag == InventoryFlag::Hangar
                    && i.type_id == p1_type
            })
            .map(|i| i.quantity)
            .sum::<i32>()
    });
    assert!(
        hangar_water >= 40,
        "Station hangar must contain at least 40 units of routed Water commodity"
    );
}

#[tokio::test]
async fn test_e6_lp_store_offerings_and_exchange() {
    // parity: plan:E-6
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut s1 = setup_pilot(&ctx, 601, "demo", "demo-secret", 90_000_001).await;
    let caldari_navy_corp_id = 1_000_035;

    // 1. GetAvailableOffersFromCorp
    let offers_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "LPStoreMgr",
            None,
            "GetAvailableOffersFromCorp",
            &[PyRep::Integer(caldari_navy_corp_id)],
            &kw,
            None,
        )
        .await
        .expect("GetAvailableOffersFromCorp");
    let PyRep::List(offers_list) = offers_rep else {
        panic!("Expected List");
    };
    assert!(!offers_list.is_empty(), "Caldari Navy LP store offers list cannot be empty");

    // Check presence of offer 1002 (Caldari Navy Scourge Light Missile, 500 LP, 500k ISK)
    let offer_1002 = offers_list.iter().find(|o| match o {
        PyRep::KeyVal(kv) => kv.get("offerID").and_then(|v| v.as_i64()) == Some(1002),
        _ => false,
    });
    assert!(offer_1002.is_some(), "Offer 1002 must be available in Caldari Navy LP store");

    // 2. Query GetLPsForCharacter
    let lps_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "LPStoreMgr",
            None,
            "GetLPsForCharacter",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetLPsForCharacter");
    let PyRep::List(lps_list) = lps_rep else {
        panic!("Expected List");
    };
    let cn_lp = lps_list.iter().find_map(|row| match row {
        PyRep::KeyVal(kv) => {
            if kv.get("corpID").and_then(|v| v.as_i64()) == Some(caldari_navy_corp_id) {
                kv.get("loyaltyPoints").and_then(|v| v.as_i64())
            } else {
                None
            }
        }
        _ => None,
    });
    assert_eq!(cn_lp, Some(5_000), "Pilot 1 starts with 5,000 LP with Caldari Navy");

    // 3. TakeOfferForCharacter: Purchase Offer 1002 (cost: 500 LP, 500,000 ISK, yields 1,000 missiles type 27361)
    let s1_bal_before = ctx.db.get_character(90_000_001).unwrap().balance;

    let take_res = ctx
        .dispatch_rpc(
            &mut s1,
            "LPStoreMgr",
            None,
            "TakeOfferForCharacter",
            &[
                PyRep::Integer(caldari_navy_corp_id),
                PyRep::Integer(1002),
                PyRep::Integer(1), // quantity
            ],
            &kw,
            None,
        )
        .await
        .expect("TakeOfferForCharacter");

    let PyRep::KeyVal(take_kv) = take_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(take_kv.get("remainingLP"), Some(&PyRep::Integer(4_500)));
    let delivered_missiles_id = take_kv.get("deliveredItemID").and_then(|v| v.as_i64()).unwrap();

    // Verify ISK deducted
    let s1_bal_after = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(s1_bal_before.0 - s1_bal_after.0, 50_000_000); // 500,000 ISK in cents

    // Verify missiles delivered to station hangar (1,000 units of type 27361)
    let missile_item = ctx.db.with_snapshot(|snap| snap.items.get(&delivered_missiles_id).cloned()).unwrap();
    assert_eq!(missile_item.type_id, 27361);
    assert_eq!(missile_item.quantity, 1_000);
    assert_eq!(missile_item.location_id, JITA_STATION_ID);
    assert_eq!(missile_item.flag, InventoryFlag::Hangar);

    // Verify wallet journal entry
    let has_lp_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal.iter().any(|j| {
            j.character_id == 90_000_001 && j.entry_type == "LPStorePurchase"
        })
    });
    assert!(has_lp_journal, "Wallet journal must record LPStorePurchase");

    // 4. Insufficient LP rejection
    // Offer 1003 (Caldari Navy Hookbill) costs 10,000 LP; pilot only has 4,500 LP
    let insufficient_lp_err = ctx
        .dispatch_rpc(
            &mut s1,
            "LPStoreMgr",
            None,
            "TakeOfferForCharacter",
            &[
                PyRep::Integer(caldari_navy_corp_id),
                PyRep::Integer(1003),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        insufficient_lp_err.code, "InsufficientLoyaltyPoints",
        "Offer costing 10,000 LP must be rejected when pilot only has 4,500 LP"
    );

    // Confirm LP and ISK balance untouched after failure
    let final_bal = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(final_bal, s1_bal_after);
    let final_lp = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&90_000_001).and_then(|c| c.loyalty_points.get(&caldari_navy_corp_id).copied())
    });
    assert_eq!(final_lp, Some(4_500));
}
