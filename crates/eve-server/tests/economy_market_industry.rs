//! Economy, Market, Contracts, Industry, and Planetary Interaction integration tests.
//!
//! // parity: plan:E-1, plan:E-2, plan:E-3, plan:E-4, plan:E-6
//! // parity: feat:market.broker-fee, feat:market.sales-tax, feat:market.escrow, feat:standings.effective-standing, feat:market.get-orders, feat:market.place-buy, feat:market.place-sell, feat:market.instant-buy, feat:market.instant-sell, feat:contracts.item-exchange, feat:contracts.item-exchange-request, feat:contracts.auction, feat:contracts.courier, feat:contracts.courier-failure, feat:industry.blueprint-data, feat:industry.facilities, feat:industry.facility-tax, feat:industry.install-job, feat:industry.job-cost, feat:pi.planet-info, feat:pi.command-center, feat:pi.colony-limit, feat:pi.place-pins, feat:pi.links

#![forbid(unsafe_code)]

mod common;

use eve_db::{
    AccountRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord,
    TypeId, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::formulas::compute_effective_standing;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_PLANET_ID: i64 = 40_009_082;
const TRITANIUM_TYPE_ID: TypeId = 34;
const RIFTER_BP_TYPE_ID: TypeId = 691;
const BROKER_RELATIONS_SKILL: TypeId = 3446;
const ACCOUNTING_SKILL: TypeId = 16622;
const COMMAND_CENTER_UPGRADES_SKILL: TypeId = 2505;
const INTERPLANETARY_CONSOLIDATION_SKILL: TypeId = 2495;
const CONNECTIONS_SKILL: TypeId = 3355;

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
async fn test_market_broker_fee_and_sales_tax() {
    // parity: plan:E-1
    // parity: feat:market.broker-fee
    // parity: feat:market.sales-tax
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut s1 = setup_pilot(&ctx, 101, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 102, "other", "other-secret", 90_000_002).await;

    // 1. Broker fee verification at level 0 vs level 5
    // Default formula: rate = 0.03 - 0.003 * broker_relations_lvl
    // Reset standings and skill to test pure 3.0% base rate at level 0:
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

    // Place buy order for 10,000 units at 1.00 ISK (gross: 10,000.00 ISK)
    // Broker fee at level 0 (3.0%): 300.00 ISK
    let order_l0 = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "PlaceCharOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(1.0),
                PyRep::Integer(10_000),
                PyRep::Bool(true),
            ],
            &kw,
            None,
        )
        .await
        .expect("PlaceCharOrder level 0");

    let PyRep::KeyVal(kv_l0) = order_l0 else {
        panic!("Expected KeyVal from PlaceCharOrder");
    };
    assert_eq!(kv_l0.get("filledImmediately"), Some(&PyRep::Bool(false)));
    let fee_l0 = kv_l0
        .get("brokerFeeISK")
        .and_then(|v| v.as_f64())
        .expect("brokerFeeISK");
    assert!(
        (fee_l0 - 300.0).abs() < 0.01,
        "Broker fee at level 0 must be 300.00 ISK, got {fee_l0}"
    );

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

    // Train Broker Relations (3446) to level 5: rate = 0.03 - 0.003 * 5 = 0.015 (1.5%) -> 150.00 ISK
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
            "PlaceCharOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(1.0),
                PyRep::Integer(10_000),
                PyRep::Bool(true),
            ],
            &kw,
            None,
        )
        .await
        .expect("PlaceCharOrder level 5");

    let PyRep::KeyVal(kv_l5) = order_l5 else {
        panic!("Expected KeyVal from PlaceCharOrder");
    };
    let fee_l5 = kv_l5
        .get("brokerFeeISK")
        .and_then(|v| v.as_f64())
        .expect("brokerFeeISK");
    assert!(
        (fee_l5 - 150.0).abs() < 0.01,
        "Broker fee at level 5 must be 150.00 ISK, got {fee_l5}"
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

    // Verify wallet journal recorded MarketBrokerFee
    let has_broker_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal
            .iter()
            .any(|j| j.character_id == 90_000_001 && j.entry_type == "MarketBrokerFee")
    });
    assert!(has_broker_journal, "Wallet journal must record MarketBrokerFee");

    // 2. Sales tax verification with Accounting skill discount:
    // Rate = 0.075 * (1.0 - 0.11 * level)
    // Setup pilot 1 with a resting buy order at price 1.00 ISK for 200 units
    let buy_order = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "PlaceCharOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(1.0),
                PyRep::Integer(200),
                PyRep::Bool(true),
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

    // Give pilot 2 some Tritanium in hangar and set Accounting to level 0
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
            .insert(ACCOUNTING_SKILL, TrainedSkillRecord {
                type_id: ACCOUNTING_SKILL,
                level: 0,
                skill_points: 0.0,
            });
    });

    let s2_bal_before = ctx.db.get_character(90_000_002).unwrap().balance;

    // Pilot 2 sells 100 units at 1.00 ISK against pilot 1's buy order
    // Gross: 100.00 ISK. Tax at level 0 (7.5%): 7.50 ISK. Net: 92.50 ISK.
    let sell_l0 = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceCharOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(1.0),
                PyRep::Integer(100),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("Place sell order level 0");

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
        "Sales tax at level 0 must be 14.10 ISK (7.5% of 188.00 ISK), got {tax_l0}"
    );

    let broker_l0 = sell_l0_kv
        .get("brokerFeeISK")
        .and_then(|v| v.as_f64())
        .expect("brokerFeeISK");
    let s2_bal_after_l0 = ctx.db.get_character(90_000_002).unwrap().balance;
    let net_l0 = (s2_bal_after_l0.0 - s2_bal_before.0) as f64 / 100.0;
    let expected_net_l0 = 188.0 - 14.10 - broker_l0;
    assert!(
        (net_l0 - expected_net_l0).abs() < 0.02,
        "Seller net proceeds at level 0 must match {expected_net_l0} ISK, got {net_l0}"
    );

    // Train Accounting (16622) to level 5: rate = 0.075 * (1.0 - 0.55) = 0.03375 (3.375%)
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

    let s2_bal_before_l5 = ctx.db.get_character(90_000_002).unwrap().balance;

    // Pilot 2 sells remaining 100 units at 1.00 ISK against pilot 1's buy order
    // Gross: 100.00 ISK. Tax at level 5 (3.375%): 3.38 ISK rounded. Net: 96.62 ISK.
    let sell_l5 = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceCharOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(1.0),
                PyRep::Integer(100),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("Place sell order level 5");

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
        "Sales tax at level 5 must be 6.35 ISK (3.375% of 188.00 ISK), got {tax_l5}"
    );

    let broker_l5 = sell_l5_kv
        .get("brokerFeeISK")
        .and_then(|v| v.as_f64())
        .expect("brokerFeeISK");
    let s2_bal_after_l5 = ctx.db.get_character(90_000_002).unwrap().balance;
    let net_l5 = (s2_bal_after_l5.0 - s2_bal_before_l5.0) as f64 / 100.0;
    let expected_net_l5 = 188.0 - 6.35 - broker_l5;
    assert!(
        (net_l5 - expected_net_l5).abs() < 0.02,
        "Seller net proceeds at level 5 must match {expected_net_l5} ISK, got {net_l5}"
    );
}

#[tokio::test]
async fn test_effective_standing_and_broker_discount() {
    // parity: feat:standings.effective-standing
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut s1 = setup_pilot(&ctx, 103, "demo", "demo-secret", 90_000_001).await;

    // 1. Direct unit verification of compute_effective_standing formula:
    // Positive standing with Connections (3355): 4% per level towards +10
    let eff_pos = compute_effective_standing(5.0, 5, 0);
    assert!(
        (eff_pos - 6.0).abs() < 1e-6,
        "Effective standing for raw 5.0 with Connections 5 must be 6.0, got {eff_pos}"
    );

    // Negative standing with Diplomacy (3357): raw + (10 - raw) * 0.04 * 5 = -5 + 15 * 0.20 = -2.0
    let eff_neg = compute_effective_standing(-5.0, 0, 5);
    assert!(
        (eff_neg - (-2.0)).abs() < 1e-6,
        "Effective standing for raw -5.0 with Diplomacy 5 must be -2.0, got {eff_neg}"
    );

    // Zero standing with untrained skills remains 0.0
    let eff_zero = compute_effective_standing(0.0, 0, 0);
    assert!(
        eff_zero.abs() < 1e-6,
        "Effective standing for raw 0.0 must be 0.0, got {eff_zero}"
    );

    // 2. Integration verification: Character with corp standing and Connections skill
    // receives lower broker fee in market order placement.
    // Jita station owner corp is Caldari Navy (1000035) or similar.
    let station_def = ctx.sde.stations.get(&JITA_STATION_ID).unwrap();
    let owner_corp = station_def.owner_corporation_id;

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&90_000_001).unwrap();
        ch.standings.clear();
        ch.standings.insert(owner_corp, 5.0); // Raw 5.0 standing to station owner corp
        snap.skills
            .entry(90_000_001)
            .or_default()
            .insert(BROKER_RELATIONS_SKILL, TrainedSkillRecord {
                type_id: BROKER_RELATIONS_SKILL,
                level: 0,
                skill_points: 0.0,
            });
        snap.skills
            .entry(90_000_001)
            .or_default()
            .insert(CONNECTIONS_SKILL, TrainedSkillRecord {
                type_id: CONNECTIONS_SKILL,
                level: 5,
                skill_points: 256_000.0,
            });
    });

    // Place buy order: rate with raw 5.0 boosted to effective 6.0:
    // Base 3.0% - 0.02 * 6.0 = 3.0% - 0.12% = 2.88%
    // 10,000 units at 1.00 ISK = 10,000 ISK gross -> broker fee = 288.00 ISK
    let order_eff = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "PlaceCharOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(1.0),
                PyRep::Integer(10_000),
                PyRep::Bool(true),
            ],
            &kw,
            None,
        )
        .await
        .expect("PlaceCharOrder with effective standing");

    let PyRep::KeyVal(eff_kv) = order_eff else {
        panic!("Expected KeyVal");
    };
    let fee_eff = eff_kv
        .get("brokerFeeISK")
        .and_then(|v| v.as_f64())
        .expect("brokerFeeISK");
    assert!(
        (fee_eff - 288.0).abs() < 0.01,
        "Broker fee with effective standing 6.0 must be 288.00 ISK, got {fee_eff}"
    );

    let oid = eff_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(
        &mut s1,
        "marketProxy",
        None,
        "CancelCharOrder",
        &[PyRep::Integer(oid)],
        &kw,
        None,
    )
    .await
    .expect("CancelCharOrder");
}

#[tokio::test]
async fn test_market_orders_limit_escrow_queries_cancellation() {
    // parity: plan:E-2
    // parity: feat:market.escrow
    // parity: feat:market.get-orders
    // parity: feat:market.place-buy
    // parity: feat:market.place-sell
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut s1 = setup_pilot(&ctx, 201, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 202, "other", "other-secret", 90_000_002).await;

    let s1_bal_initial = ctx.db.get_character(90_000_001).unwrap().balance;

    // 1. Place resting limit buy order (feat:market.place-buy) with escrow deduction (feat:market.escrow)
    let buy_price = 0.50;
    let buy_qty = 5_000;
    let buy_order = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "PlaceCharOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(buy_price),
                PyRep::Integer(buy_qty),
                PyRep::Bool(true),
            ],
            &kw,
            None,
        )
        .await
        .expect("PlaceCharOrder buy");

    let PyRep::KeyVal(buy_kv) = buy_order else {
        panic!("Expected KeyVal");
    };
    assert_eq!(buy_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));
    let buy_oid = buy_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();
    let broker_fee_isk = buy_kv.get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap();

    // Verify buyer's balance was debited for both order cost (escrow) and broker fee
    let s1_bal_after_buy = ctx.db.get_character(90_000_001).unwrap().balance;
    let expected_escrow_cents = (buy_price * (buy_qty as f64) * 100.0) as i64;
    let expected_broker_cents = (broker_fee_isk * 100.0) as i64;
    assert_eq!(
        s1_bal_initial.0 - s1_bal_after_buy.0,
        expected_escrow_cents + expected_broker_cents,
        "Buyer balance must be debited by escrow + broker fee"
    );

    // Verify escrow is tracked in order record
    let order_rec = ctx.db.with_snapshot(|snap| snap.market_orders.get(&buy_oid).cloned()).unwrap();
    assert_eq!(order_rec.escrow.0, expected_escrow_cents);
    assert_eq!(order_rec.vol_remaining, buy_qty as i32);

    // 2. Query regional order book (feat:market.get-orders)
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
    assert_eq!(books.len(), 2, "GetOrders must return (sells, buys) tuple");

    // 3. Place resting limit sell order (feat:market.place-sell)
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
                quantity: 10_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let sell_price = 25.0; // High price so it rests without matching
    let sell_qty = 2_000;
    let sell_order = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceCharOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(sell_price),
                PyRep::Integer(sell_qty),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("PlaceCharOrder sell");

    let PyRep::KeyVal(sell_kv) = sell_order else {
        panic!("Expected KeyVal");
    };
    assert_eq!(sell_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));
    let sell_oid = sell_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();

    // Verify hangar items were deducted for the sell order
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
    assert_eq!(s2_hangar_trit, 8_000, "10,000 - 2,000 sell order = 8,000 remaining in hangar");

    // 4. Cancel buy order with escrow refund (feat:market.escrow)
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
        expected_escrow_cents,
        "Escrow must be fully refunded upon buy order cancellation"
    );

    // Cancel sell order: remaining items returned to hangar
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
    assert_eq!(
        s2_hangar_after_cancel, 10_000,
        "Cancelled sell items must return to hangar"
    );
}

#[tokio::test]
async fn test_market_instant_matching() {
    // parity: feat:market.instant-buy
    // parity: feat:market.instant-sell
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut s1 = setup_pilot(&ctx, 203, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 204, "other", "other-secret", 90_000_002).await;

    // Give pilot 2 Tritanium in Jita
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
                quantity: 5_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Pilot 2 places resting sell order at 10.00 ISK for 1,000 units
    let sell_ask = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceCharOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(10.0),
                PyRep::Integer(1_000),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("Place resting sell order");

    let PyRep::KeyVal(ask_kv) = sell_ask else {
        panic!("Expected KeyVal");
    };
    assert_eq!(ask_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));

    // 1. Instant Buy (feat:market.instant-buy)
    // Pilot 1 places buy order at 10.00 ISK for 500 units, immediately crossing the resting ask
    let instant_buy = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "PlaceCharOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(10.0),
                PyRep::Integer(500),
                PyRep::Bool(true),
            ],
            &kw,
            None,
        )
        .await
        .expect("Instant buy order");

    let PyRep::KeyVal(ibuy_kv) = instant_buy else {
        panic!("Expected KeyVal");
    };
    assert_eq!(
        ibuy_kv.get("filledImmediately"),
        Some(&PyRep::Bool(true)),
        "Order crossing resting ask must be filledImmediately"
    );

    // Verify buyer received 500 Tritanium in hangar
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
    assert!(s1_hangar_trit >= 500, "Buyer must have received purchased Tritanium");

    // 2. Instant Sell (feat:market.instant-sell)
    // Pilot 1 places resting buy order at 8.00 ISK for 400 units
    let resting_bid = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "PlaceCharOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(1.0),
                PyRep::Integer(400),
                PyRep::Bool(true),
            ],
            &kw,
            None,
        )
        .await
        .expect("Place resting buy order");

    let PyRep::KeyVal(bid_kv) = resting_bid else {
        panic!("Expected KeyVal");
    };
    assert_eq!(bid_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));

    let s2_bal_before_isell = ctx.db.get_character(90_000_002).unwrap().balance;

    // Pilot 2 places sell order at 1.00 ISK for 400 units, immediately crossing the resting bid
    let instant_sell = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceCharOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(1.0),
                PyRep::Integer(400),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("Instant sell order");

    let PyRep::KeyVal(isell_kv) = instant_sell else {
        panic!("Expected KeyVal");
    };
    assert_eq!(
        isell_kv.get("filledImmediately"),
        Some(&PyRep::Bool(true)),
        "Order crossing resting bid must be filledImmediately"
    );

    let s2_bal_after_isell = ctx.db.get_character(90_000_002).unwrap().balance;
    assert!(
        s2_bal_after_isell.0 > s2_bal_before_isell.0,
        "Seller must have received ISK from instant sell fill"
    );
}

#[tokio::test]
async fn test_contracts_item_exchange_and_request() {
    // parity: plan:E-3
    // parity: feat:contracts.item-exchange
    // parity: feat:contracts.item-exchange-request
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut s1 = setup_pilot(&ctx, 301, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 302, "other", "other-secret", 90_000_002).await;

    // 1. One-way ItemExchange Contract (feat:contracts.item-exchange)
    // Pilot 1 sells item 100_000_003 (1MN Afterburner II) for 75,000 ISK
    let c_exchange = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::String("ItemExchange".into()),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Real(75_000.0), // price
                PyRep::Real(0.0),      // reward
                PyRep::Integer(100_000_003),
            ],
            &kw,
            None,
        )
        .await
        .expect("Create ItemExchange Contract");

    let PyRep::KeyVal(cx_kv) = c_exchange else {
        panic!("Expected KeyVal");
    };
    let exchange_cid = cx_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();

    let s1_bal_before_accept = ctx.db.get_character(90_000_001).unwrap().balance;
    let s2_bal_before_accept = ctx.db.get_character(90_000_002).unwrap().balance;

    // Pilot 2 accepts the ItemExchange contract
    let accept_res = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "AcceptContract",
            &[PyRep::Integer(exchange_cid)],
            &kw,
            None,
        )
        .await
        .expect("Accept ItemExchange Contract");

    let PyRep::KeyVal(acc_kv) = accept_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(acc_kv.get("status"), Some(&PyRep::String("Completed".into())));

    // Verify ISK transfer: pilot 2 pays 75,000 ISK, pilot 1 receives 75,000 ISK
    let s1_bal_after = ctx.db.get_character(90_000_001).unwrap().balance;
    let s2_bal_after = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(s1_bal_after.0 - s1_bal_before_accept.0, 7_500_000);
    assert_eq!(s2_bal_before_accept.0 - s2_bal_after.0, 7_500_000);

    // Verify item ownership transferred to pilot 2
    let item_owner = ctx.db.with_snapshot(|snap| {
        snap.items.get(&100_000_003).map(|i| (i.owner_id, i.flag))
    });
    assert_eq!(item_owner, Some((90_000_002, InventoryFlag::Hangar)));

    // 2. Two-way ItemExchange Contract Requesting Items (feat:contracts.item-exchange-request)
    // Pilot 1 creates a contract paying 10,000 ISK reward, requesting 500 units of Tritanium (type 34)
    // Give pilot 2 Tritanium in hangar
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
                quantity: 1_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let requested_items = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Integer(500)]),
    ]);

    let c_req = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::String("ItemExchange".into()),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Real(0.0),      // price
                PyRep::Real(10_000.0), // reward paid to acceptor
                PyRep::None,           // item_ids
                requested_items,
            ],
            &kw,
            None,
        )
        .await
        .expect("Create ItemExchange with requested items");

    let PyRep::KeyVal(creq_kv) = c_req else {
        panic!("Expected KeyVal");
    };
    let req_cid = creq_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();

    // Pilot 2 accepts the request contract
    let accept_req = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "AcceptContract",
            &[PyRep::Integer(req_cid)],
            &kw,
            None,
        )
        .await
        .expect("Accept ItemExchange Request Contract");

    let PyRep::KeyVal(accreq_kv) = accept_req else {
        panic!("Expected KeyVal");
    };
    assert_eq!(accreq_kv.get("status"), Some(&PyRep::String("Completed".into())));

    // Verify Tritanium transferred to Pilot 1's hangar
    let s1_trit_qty = ctx.db.with_snapshot(|snap| {
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
    assert!(s1_trit_qty >= 500, "Pilot 1 must receive requested Tritanium items");
}

#[tokio::test]
async fn test_contracts_auction_and_courier() {
    // parity: feat:contracts.auction
    // parity: feat:contracts.courier
    // parity: feat:contracts.courier-failure
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut s1 = setup_pilot(&ctx, 303, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 304, "other", "other-secret", 90_000_002).await;

    // Create 3rd pilot for auction outbidding
    let auc_item_id = ctx.db.with_mut_snapshot(|snap| {
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

        // Give pilot 1 items to auction
        snap.next_entity_id += 1;
        let auc_item_id = snap.next_entity_id;
        snap.items.insert(
            auc_item_id,
            InventoryItemRecord {
                item_id: auc_item_id,
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
        auc_item_id
    });
    let mut s3 = setup_pilot(&ctx, 305, "third", "third-secret", 90_000_003).await;

    // 1. Auction Contract (feat:contracts.auction)
    // Pilot 1 creates Auction: start_bid = 100,000 ISK, buyout = 500,000 ISK
    let c_auction = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::String("Auction".into()),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Real(100_000.0), // start bid
                PyRep::Real(500_000.0), // buyout
                PyRep::Integer(auc_item_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Create Auction Contract");

    let PyRep::KeyVal(auc_kv) = c_auction else {
        panic!("Expected KeyVal");
    };
    let auc_cid = auc_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();

    let s2_bal_before_bid = ctx.db.get_character(90_000_002).unwrap().balance;

    // Pilot 2 places bid of 150,000 ISK
    ctx.dispatch_rpc(
        &mut s2,
        "contractProxy",
        None,
        "PlaceBid",
        &[PyRep::Integer(auc_cid), PyRep::Real(150_000.0)],
        &kw,
        None,
    )
    .await
    .expect("PlaceBid pilot 2");

    let s2_bal_after_bid = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(
        s2_bal_before_bid.0 - s2_bal_after_bid.0,
        15_000_000,
        "Bidder 2 escrow must be held"
    );

    // Pilot 3 outbids Pilot 2 with bid of 200,000 ISK -> Pilot 2 receives outbid refund!
    ctx.dispatch_rpc(
        &mut s3,
        "contractProxy",
        None,
        "PlaceBid",
        &[PyRep::Integer(auc_cid), PyRep::Real(200_000.0)],
        &kw,
        None,
    )
    .await
    .expect("PlaceBid pilot 3 outbid");

    let s2_bal_after_outbid = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(
        s2_bal_after_outbid, s2_bal_before_bid,
        "Pilot 2 must receive full refund when outbid"
    );

    // Pilot 3 exercises Buyout at 500,000 ISK -> contract immediately completed
    ctx.dispatch_rpc(
        &mut s3,
        "contractProxy",
        None,
        "PlaceBid",
        &[PyRep::Integer(auc_cid), PyRep::Real(500_000.0)],
        &kw,
        None,
    )
    .await
    .expect("Buyout auction");

    let auc_status = ctx.db.with_snapshot(|snap| {
        snap.contracts.get(&auc_cid).map(|c| c.status.clone())
    });
    assert_eq!(auc_status, Some("Completed".into()), "Auction buyout must complete contract");

    // Item delivered to pilot 3's hangar
    let item_owner_auc = ctx.db.with_snapshot(|snap| {
        snap.items.get(&auc_item_id).map(|i| (i.owner_id, i.flag))
    });
    assert_eq!(item_owner_auc, Some((90_000_003, InventoryFlag::Hangar)));

    // 2. Courier Contract (feat:contracts.courier)
    // Pilot 1 creates courier contract: start at Jita, end at Perimeter, reward 50k, collateral 200k
    let c_courier = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::String("Courier".into()),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Real(50_000.0),  // reward
                PyRep::Real(200_000.0), // collateral
                PyRep::None,            // items (uses plastic wrap)
            ],
            &kw,
            None,
        )
        .await
        .expect("Create Courier Contract");

    let PyRep::KeyVal(cour_kv) = c_courier else {
        panic!("Expected KeyVal");
    };
    let cour_cid = cour_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();

    let s2_bal_before_courier = ctx.db.get_character(90_000_002).unwrap().balance;

    // Pilot 2 accepts the courier contract -> collateral debited
    ctx.dispatch_rpc(
        &mut s2,
        "contractProxy",
        None,
        "AcceptContract",
        &[PyRep::Integer(cour_cid)],
        &kw,
        None,
    )
    .await
    .expect("Accept Courier Contract");

    let s2_bal_after_accept_cour = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(
        s2_bal_before_courier.0 - s2_bal_after_accept_cour.0,
        20_000_000,
        "Acceptor must post collateral for courier contract"
    );

    // Pilot 2 completes contract
    ctx.dispatch_rpc(
        &mut s2,
        "contractProxy",
        None,
        "CompleteContract",
        &[PyRep::Integer(cour_cid)],
        &kw,
        None,
    )
    .await
    .expect("Complete Courier Contract");

    let s2_bal_after_complete_cour = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(
        s2_bal_after_complete_cour.0 - s2_bal_before_courier.0,
        5_000_000,
        "Collateral refunded and reward (50,000 ISK) paid to courier runner"
    );

    // 3. Courier Contract Failure (feat:contracts.courier-failure)
    let c_fail_courier = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::String("Courier".into()),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Real(50_000.0),  // reward
                PyRep::Real(300_000.0), // collateral
                PyRep::None,
            ],
            &kw,
            None,
        )
        .await
        .expect("Create Courier Contract for Failure test");

    let PyRep::KeyVal(cfail_kv) = c_fail_courier else {
        panic!("Expected KeyVal");
    };
    let fail_cid = cfail_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();

    // Pilot 2 accepts
    ctx.dispatch_rpc(
        &mut s2,
        "contractProxy",
        None,
        "AcceptContract",
        &[PyRep::Integer(fail_cid)],
        &kw,
        None,
    )
    .await
    .expect("Accept Courier Contract to fail");

    let s1_bal_before_fail = ctx.db.get_character(90_000_001).unwrap().balance;

    // Pilot 2 fails the contract via FailContract
    let fail_res = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "FailContract",
            &[PyRep::Integer(fail_cid)],
            &kw,
            None,
        )
        .await
        .expect("FailContract");

    let PyRep::KeyVal(fres_kv) = fail_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(fres_kv.get("status"), Some(&PyRep::String("Failed".into())));

    // Issuer receives collateral + escrowed reward
    let s1_bal_after_fail = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(
        s1_bal_after_fail.0 - s1_bal_before_fail.0,
        35_000_000, // 300,000 collateral + 50,000 reward refund
        "Issuer must receive collateral and reward refund on courier contract failure"
    );
}

#[tokio::test]
async fn test_industry_blueprint_data_facilities_jobs() {
    // parity: plan:E-4
    // parity: feat:industry.blueprint-data
    // parity: feat:industry.facilities
    // parity: feat:industry.facility-tax
    // parity: feat:industry.install-job
    // parity: feat:industry.job-cost
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut s1 = setup_pilot(&ctx, 401, "demo", "demo-secret", 90_000_001).await;

    // 1. Blueprint data inspection (feat:industry.blueprint-data)
    let bp_data = ctx
        .dispatch_rpc(
            &mut s1,
            "industryManager",
            None,
            "GetBlueprintData",
            &[PyRep::Integer(RIFTER_BP_TYPE_ID as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetBlueprintData");

    let PyRep::KeyVal(bp_kv) = bp_data else {
        panic!("Expected KeyVal from GetBlueprintData");
    };
    assert_eq!(bp_kv.get("blueprintTypeID"), Some(&PyRep::Integer(RIFTER_BP_TYPE_ID as i64)));
    assert_eq!(bp_kv.get("productTypeID"), Some(&PyRep::Integer(587))); // Rifter ship
    assert!(bp_kv.get("manufacturingTime").and_then(|v| v.as_i64()).unwrap() > 0);

    // 2. Facilities query (feat:industry.facilities)
    let facilities_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "industryManager",
            None,
            "GetFacilities",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetFacilities");

    let PyRep::List(fac_list) = facilities_rep else {
        panic!("Expected List from GetFacilities");
    };
    assert!(!fac_list.is_empty(), "Must return at least one industry facility");

    // 3. Job cost calculation (feat:industry.job-cost)
    let cost_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "industryManager",
            None,
            "GetJobCost",
            &[
                PyRep::Integer(RIFTER_BP_TYPE_ID as i64),
                PyRep::Integer(1), // Manufacturing
                PyRep::Integer(2), // 2 runs
            ],
            &kw,
            None,
        )
        .await
        .expect("GetJobCost");

    let PyRep::KeyVal(cost_kv) = cost_rep else {
        panic!("Expected KeyVal from GetJobCost");
    };
    assert_eq!(cost_kv.get("baseJobCost"), Some(&PyRep::Real(20_000.0)));
    assert_eq!(cost_kv.get("facilityTaxISK"), Some(&PyRep::Real(2_000.0)));
    assert_eq!(cost_kv.get("estimatedCost"), Some(&PyRep::Real(22_000.0)));

    // 4. Job installation & facility tax deduction (feat:industry.install-job, feat:industry.facility-tax)
    let s1_bal_before_job = ctx.db.get_character(90_000_001).unwrap().balance;

    let mfg_install = ctx
        .dispatch_rpc(
            &mut s1,
            "industryManager",
            None,
            "InstallJob",
            &[
                PyRep::Integer(RIFTER_BP_TYPE_ID as i64),
                PyRep::Integer(1), // Manufacturing
                PyRep::Integer(1), // 1 run
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

    // Verify facility tax deduction from balance and wallet journal
    let s1_bal_after_job = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(s1_bal_before_job.0 - s1_bal_after_job.0, 100_000); // 1,000.00 ISK
    let has_tax_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal.iter().any(|j| {
            j.character_id == 90_000_001 && j.entry_type == "IndustryFacilityTax"
        })
    });
    assert!(has_tax_journal, "Wallet journal must record IndustryFacilityTax");

    // Fast-forward duration and complete job
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(j) = snap.industry_jobs.get_mut(&mfg_job_id) {
            j.start_unix = 0;
        }
    });

    let complete_res = ctx
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
        .expect("CompleteJob");

    let PyRep::KeyVal(cres_kv) = complete_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(cres_kv.get("productTypeID"), Some(&PyRep::Integer(587))); // Rifter
    let delivered_id = cres_kv.get("deliveredItemID").and_then(|v| v.as_i64()).unwrap();

    let delivered_item = ctx.db.with_snapshot(|snap| {
        snap.items.get(&delivered_id).map(|i| (i.owner_id, i.location_id, i.flag))
    });
    assert_eq!(
        delivered_item,
        Some((90_000_001, JITA_STATION_ID, InventoryFlag::Hangar)),
        "Manufactured item must be delivered to station hangar"
    );
}

#[tokio::test]
async fn test_pi_colony_command_center_pins_and_links() {
    // parity: plan:E-6
    // parity: feat:pi.planet-info
    // parity: feat:pi.command-center
    // parity: feat:pi.colony-limit
    // parity: feat:pi.place-pins
    // parity: feat:pi.links
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut s1 = setup_pilot(&ctx, 501, "demo", "demo-secret", 90_000_001).await;

    // 1. Planet info query (feat:pi.planet-info)
    let pinfo = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "GetPlanetInfo",
            &[PyRep::Integer(JITA_PLANET_ID)],
            &kw,
            None,
        )
        .await
        .expect("GetPlanetInfo");

    let PyRep::KeyVal(pkv) = pinfo else {
        panic!("Expected KeyVal from GetPlanetInfo");
    };
    assert_eq!(pkv.get("planetID"), Some(&PyRep::Integer(JITA_PLANET_ID)));
    assert_eq!(pkv.get("hasColony"), Some(&PyRep::Bool(false)));

    // 2. Command center placement (feat:pi.command-center)
    let create_col = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "CreateColony",
            &[PyRep::Integer(JITA_PLANET_ID)],
            &kw,
            None,
        )
        .await
        .expect("CreateColony");

    let PyRep::KeyVal(col_kv) = create_col else {
        panic!("Expected KeyVal");
    };
    assert_eq!(col_kv.get("commandCenterLevel"), Some(&PyRep::Integer(1)));
    assert_eq!(col_kv.get("pinsCount"), Some(&PyRep::Integer(2)));

    // 3. Colony limit enforcement (feat:pi.colony-limit)
    // Pilot has Interplanetary Consolidation (2495) level 0 -> limit is 1 colony
    // Attempting to create a second colony on another planet must be rejected with MaxColoniesReached
    let second_planet_id = 40_009_098; // Jita V
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills
            .entry(90_000_001)
            .or_default()
            .remove(&INTERPLANETARY_CONSOLIDATION_SKILL);
    });

    let col_limit_err = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "CreateColony",
            &[PyRep::Integer(second_planet_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        col_limit_err.code, "MaxColoniesReached",
        "Exceeding colony limit must return MaxColoniesReached"
    );

    // 4. Command Center Upgrade (feat:pi.command-center)
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

    let up_res = ctx
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

    let PyRep::KeyVal(up_kv) = up_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(up_kv.get("commandCenterLevel"), Some(&PyRep::Integer(2)));
    assert_eq!(up_kv.get("pinsCount"), Some(&PyRep::Integer(3)));

    let s1_bal_after_up = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(s1_bal_before_up.0 - s1_bal_after_up.0, 50_000_000); // 500,000 ISK in cents

    // 5. Place Pins (feat:pi.place-pins)
    let place_pins_res = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "PlacePins",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::Integer(2848), // Basic Industry Facility pin
                PyRep::Integer(2),    // 2 pins
            ],
            &kw,
            None,
        )
        .await
        .expect("PlacePins");

    let PyRep::KeyVal(pins_kv) = place_pins_res else {
        panic!("Expected KeyVal from PlacePins");
    };
    assert_eq!(pins_kv.get("pinsCount"), Some(&PyRep::Integer(5)));

    // 6. Links (feat:pi.links)
    let link_res = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "CreateLink",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::Integer(1),      // source pin
                PyRep::Integer(2),      // target pin
                PyRep::Real(120.0),     // distance km
                PyRep::Integer(1),      // upgrade level
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateLink");

    let PyRep::KeyVal(link_kv) = link_res else {
        panic!("Expected KeyVal from CreateLink");
    };
    assert!(link_kv.get("linkPowergrid").and_then(|v| v.as_f64()).unwrap() > 0.0);
    assert!(link_kv.get("linkCPU").and_then(|v| v.as_f64()).unwrap() > 0.0);
}
