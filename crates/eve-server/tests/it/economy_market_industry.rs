//! Economy, Market, Contracts, Industry, and Planetary Interaction integration tests.
//!
//! // parity: plan:E-1, plan:E-2, plan:E-3, plan:E-4, plan:E-6
//! // parity: feat:market.broker-fee, feat:market.sales-tax, feat:market.escrow, feat:standings.effective-standing, feat:market.get-orders, feat:market.place-buy, feat:market.place-sell, feat:market.instant-buy, feat:market.instant-sell, feat:contracts.item-exchange, feat:contracts.item-exchange-request, feat:contracts.auction, feat:contracts.courier, feat:contracts.courier-failure, feat:industry.blueprint-data, feat:industry.facilities, feat:industry.facility-tax, feat:industry.install-job, feat:industry.job-cost, feat:pi.planet-info, feat:pi.command-center, feat:pi.colony-limit, feat:pi.place-pins, feat:pi.links

#![forbid(unsafe_code)]

use crate::common;

use eve_db::{
    AccountRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    TrainedSkillRecord, TypeId, UpwellStructureRecord, WorldRepository,
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
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(1.0),
                PyRep::Integer(10_000),
                PyRep::Integer(90),
                PyRep::Integer(1),
                PyRep::Bool(false),
                PyRep::Bool(false),
                PyRep::None,
                PyRep::Real(0.0),
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
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(1.0),
                PyRep::Integer(10_000),
                PyRep::Integer(90),
                PyRep::Integer(1),
                PyRep::Bool(false),
                PyRep::Bool(false),
                PyRep::None,
                PyRep::Real(0.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("PlaceBuyOrder level 5");

    let PyRep::KeyVal(kv_l5) = order_l5 else {
        panic!("Expected KeyVal from PlaceBuyOrder");
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
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(1.0),
                PyRep::Integer(200),
                PyRep::Integer(90),
                PyRep::Integer(1),
                PyRep::Bool(false),
                PyRep::Bool(false),
                PyRep::None,
                PyRep::Real(0.0),
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

    // Pilot 2 sells 100 units at 1.00 ISK against existing resting buy orders via PlaceMultiSellOrder
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
        .expect("PlaceMultiSellOrder level 0");

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

    // Pilot 2 sells remaining 100 units at 1.00 ISK
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
        .expect("PlaceMultiSellOrder level 5");

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
    let has_tax_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal
            .iter()
            .any(|j| j.character_id == 90_000_002 && j.entry_type == "TransactionTax")
    });
    assert!(
        has_tax_journal,
        "Wallet journal must record TransactionTax for seller sales tax"
    );

    // 3. Player structure broker fee rate (feat:market.broker-fee):
    // Seed an online Upwell Fortizar in Jita owned by corp 98_000_777 with a custom 2.5% rate
    let struct_id = 1_000_000_777_001_i64;
    let struct_corp_id = ctx.db.with_mut_snapshot(|snap| {
        let corp_id = snap.characters[&90_000_001].corporation_id;
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Market Hub Corp".into(),
                ticker: "MHUB".into(),
                ceo_id: 90_000_001,
                master_wallet: IskCents(0),
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35833, // Fortizar
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142, // Jita
                name: "Jita Market Fortizar".into(),
                position: [1_000_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: 5_000.0,
                armor_hp: 5_000.0,
                hull_hp: 5_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        corp_id
    });
    ctx.dispatch_rpc(
        &mut s1,
        "facilityManager",
        None,
        "SetFacilityTaxes",
        &[
            PyRep::Integer(struct_id),
            PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.025))]),
        ],
        &kw,
        None,
    )
    .await
    .expect("Set structure broker rate");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
            ch.structure_id = Some(struct_id);
        }
    });
    s1.station_id = None;
    s1.structure_id = Some(struct_id);
    let struct_order = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(struct_id),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(1.0),
                PyRep::Integer(10_000),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(90),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("PlaceBuyOrder at Upwell structure");
    let PyRep::KeyVal(struct_kv) = struct_order else {
        panic!("Expected KeyVal");
    };
    let struct_fee = struct_kv
        .get("brokerFeeISK")
        .and_then(|v| v.as_f64())
        .expect("brokerFeeISK");
    assert!(
        (struct_fee - 250.0).abs() < 0.01,
        "Upwell structure broker fee at 2.5% on 10,000 ISK order must be 250.00 ISK, got {struct_fee}"
    );
    let corp_wallet = ctx
        .db
        .with_snapshot(|snap| snap.corporations[&struct_corp_id].master_wallet);
    assert_eq!(
        corp_wallet,
        IskCents(12_500),
        "Structure owner corporation must receive 50% share (125.00 ISK) of structure broker fee"
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

    // Place buy order: NPC station broker fee uses raw unmodified standing (5.0),
    // never Connections/Diplomacy effective standing (`wiki:Broker_fee_and_sales_tax`):
    // Base 3.0% - 0.02% * 5.0 = 3.0% - 0.10% = 2.90%
    // 10,000 units at 1.00 ISK = 10,000 ISK gross -> broker fee = 290.00 ISK
    let order_eff = ctx
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
                PyRep::Bool(false),
                PyRep::Bool(false),
                PyRep::None,
                PyRep::Real(0.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("PlaceBuyOrder with raw corp standing");

    let PyRep::KeyVal(eff_kv) = order_eff else {
        panic!("Expected KeyVal");
    };
    let fee_eff = eff_kv
        .get("brokerFeeISK")
        .and_then(|v| v.as_f64())
        .expect("brokerFeeISK");
    assert!(
        (fee_eff - 290.0).abs() < 0.01,
        "Broker fee with raw corp standing 5.0 (unaffected by Connections 5) must be 290.00 ISK, got {fee_eff}"
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
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(buy_price),
                PyRep::Integer(buy_qty),
                PyRep::Integer(5),   // orderRange = 5 jumps
                PyRep::Integer(100), // minVolume = 100
                PyRep::Integer(30),  // duration = 30 days
                PyRep::Bool(false),  // useCorp = false
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

    // Verify buyer's balance was debited for both order cost (escrow) and broker fee
    let s1_bal_after_buy = ctx.db.get_character(90_000_001).unwrap().balance;
    let expected_escrow_cents = (buy_price * (buy_qty as f64) * 100.0) as i64;
    let expected_broker_cents = (broker_fee_isk * 100.0) as i64;
    assert_eq!(
        s1_bal_initial.0 - s1_bal_after_buy.0,
        expected_escrow_cents + expected_broker_cents,
        "Buyer balance must be debited by escrow + broker fee"
    );

    // Verify escrow is tracked in order record and exposed via GetCharEscrow
    let order_rec = ctx.db.with_snapshot(|snap| snap.market_orders.get(&buy_oid).cloned()).unwrap();
    assert_eq!(order_rec.escrow.0, expected_escrow_cents);
    assert_eq!(order_rec.vol_remaining, buy_qty as i32);
    assert_eq!(order_rec.min_volume, 100);

    let escrow_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "GetCharEscrow",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetCharEscrow");
    let PyRep::KeyVal(escrow_kv) = escrow_rep else {
        panic!("Expected KeyVal from GetCharEscrow");
    };
    assert_eq!(
        escrow_kv.get("totalEscrowISK"),
        Some(&PyRep::Real(2_500.0)),
        "GetCharEscrow must report active buy order escrow"
    );

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
    assert_eq!(buy_row_cols.get(0).map(|(_, v)| v), Some(&PyRep::Real(0.50))); // price
    assert_eq!(buy_row_cols.get(1).map(|(_, v)| v), Some(&PyRep::Real(5_000.0))); // volRemaining
    assert_eq!(
        buy_row_cols.get(2).map(|(_, v)| v),
        Some(&PyRep::Integer(TRITANIUM_TYPE_ID as i64))
    ); // typeID
    assert_eq!(buy_row_cols.get(4).map(|(_, v)| v), Some(&PyRep::Integer(5_000))); // volEntered
    assert_eq!(buy_row_cols.get(5).map(|(_, v)| v), Some(&PyRep::Integer(100))); // minVolume
    assert_eq!(buy_row_cols.get(6).map(|(_, v)| v), Some(&PyRep::Bool(true))); // bid
    assert_eq!(
        buy_row_cols.get(7).map(|(_, v)| v),
        Some(&PyRep::Integer(JITA_STATION_ID))
    ); // stationID
    assert_eq!(buy_row_cols.get(9).map(|(_, v)| v), Some(&PyRep::Integer(5))); // range (5 jumps)
    assert!(
        buy_row_cols
            .get(10)
            .and_then(|(_, c)| c.as_i64())
            .unwrap_or(0)
            > 0
    ); // issueDate
    assert_eq!(buy_row_cols.get(11).map(|(_, v)| v), Some(&PyRep::Integer(30))); // duration (30 days)

    // 3. Place resting limit sell order & PlaceMultiSellOrder (feat:market.place-sell)
    // Give pilot 2 some Tritanium and Pyerite (type 35) + an assembled Velator (606) in hangar
    let (pye_item_id, assembled_ship_id) = ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_002).or_default().remove(&3443); // untrained Trade -> base openOrderLimit = 5
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

    // Attempting to sell assembled (singleton=true) ship must be rejected
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
    assert_eq!(
        assembled_err.unwrap_err().code,
        "CannotSellAssembledItem",
        "Assembled items must be repackaged before selling on the market"
    );

    let sell_price = 25.0; // High price so it rests without matching
    let sell_qty = 2_000;
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

    // Verify PlaceMultiSellOrder atomicity: if a later entry fails (e.g., assembled ship), earlier entries roll back
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
    assert_eq!(
        pye_qty_after_rollback, 1_000,
        "PlaceMultiSellOrder must roll back earlier item deductions when a later entry fails"
    );

    // Verify useCorp=true without corporation trader/accountant role is rejected
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
    assert!(
        matches!(
            corp_err.unwrap_err().code.as_str(),
            "CorporationNotFound" | "CorpRoleRequired"
        ),
        "useCorp=true without corp wallet/role must be rejected"
    );

    // Verify expectedBrokersFee slippage check rejects when actual broker fee (100 ISK floor) > expectedBrokersFee (10 ISK)
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
    assert_eq!(
        fee_exceeded_err.unwrap_err().code,
        "BrokerFeeExceeded",
        "PlaceMultiSellOrder must reject when actual broker fee exceeds expectedBrokersFee"
    );

    // Exercise PlaceMultiSellOrder(itemList, useCorp, duration, expectedBrokersFee) per V24 marketsvc.pyj / sellMulti.pyj
    let multi_sell = ctx
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
                        ("typeID", PyRep::Integer(35)),
                        ("itemID", PyRep::Integer(pye_item_id)),
                        ("price", PyRep::Real(13.0)),
                        ("quantity", PyRep::Integer(200)),
                    ]),
                    PyRep::keyval([
                        ("stationID", PyRep::Integer(JITA_STATION_ID)),
                        ("typeID", PyRep::Integer(35)),
                        ("itemID", PyRep::Integer(pye_item_id)),
                        ("price", PyRep::Real(14.0)),
                        ("quantity", PyRep::Integer(200)),
                    ]),
                    PyRep::keyval([
                        ("stationID", PyRep::Integer(JITA_STATION_ID)),
                        ("typeID", PyRep::Integer(35)),
                        ("itemID", PyRep::Integer(pye_item_id)),
                        ("price", PyRep::Real(15.0)),
                        ("quantity", PyRep::Integer(200)),
                    ]),
                ]),
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
        panic!("Expected KeyVal from PlaceMultiSellOrder");
    };
    assert_eq!(ms_kv.get("placedCount"), Some(&PyRep::Integer(4)));
    assert_eq!(
        ms_kv.get("totalBrokerFeeISK"),
        Some(&PyRep::Real(400.0)),
        "4 resting sell orders each below 100 ISK raw fee must pay 4 * 100.0 ISK minimum broker fee"
    );

    // Pilot 2 now has 1 + 4 = 5 open resting orders (base openOrderLimit = 5 with untrained Trade skill).
    // Placing a 6th resting order must fail with MaxOpenOrdersReached:
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
    assert_eq!(
        limit_err.unwrap_err().code,
        "MaxOpenOrdersReached",
        "Must enforce openOrderLimit from Trade/Retail/Wholesale/Tycoon skills"
    );

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

    // Verify Margin Trading (skill 16597) partial escrow factor (0.75^level):
    // At level 4, escrow factor is 0.75^4 = 0.31640625 -> 10,000 ISK order holds 3,164.06 ISK in escrow
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
                PyRep::Real(1.0),
                PyRep::Integer(10_000),
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
    assert_eq!(
        mt_escrow,
        IskCents(316_406),
        "Margin Trading 4 must reduce required escrow on 10,000 ISK order to 3,164.06 ISK"
    );
    ctx.dispatch_rpc(
        &mut s1,
        "marketProxy",
        None,
        "CancelCharOrder",
        &[PyRep::Integer(mt_oid)],
        &kw,
        None,
    )
    .await
    .expect("Cancel Margin Trading buy order");

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

    // Give pilot 2 Tritanium in Jita and clear pre-seeded NPC Tritanium orders so s1 matches s2's asks directly
    let s2_trit_id = ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_002).or_default().remove(&16622); // untrained Accounting -> 7.5% sales tax
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

    // Pilot 2 places two resting sell orders at 10.00 ISK (1,000 units) and 12.00 ISK (500 units) via PlaceMultiSellOrder
    let sell_ask = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
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
            &kw,
            None,
        )
        .await
        .expect("Place resting sell orders");

    let PyRep::KeyVal(ask_kv) = sell_ask else {
        panic!("Expected KeyVal");
    };
    assert_eq!(ask_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));
    assert_eq!(ask_kv.get("placedCount"), Some(&PyRep::Integer(2)));

    let s2_bal_before_fill = ctx.db.get_character(90_000_002).unwrap().balance;

    // 1. Instant Buy & BuyMultipleItems (feat:market.instant-buy)
    // Pilot 1 places immediate buy order (duration = 0) at 10.00 ISK for 500 units via PlaceBuyOrder
    let instant_buy = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(10.0),
                PyRep::Integer(500),
                PyRep::Integer(-1), // orderRange = station
                PyRep::Integer(1),  // minVolume = 1
                PyRep::Integer(0),  // duration = 0 (immediate order)
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("Instant PlaceBuyOrder");

    let PyRep::KeyVal(ibuy_kv) = instant_buy else {
        panic!("Expected KeyVal");
    };
    assert_eq!(
        ibuy_kv.get("filledImmediately"),
        Some(&PyRep::Bool(true)),
        "Order crossing resting ask must be filledImmediately"
    );

    // Verify seller received net proceeds (5,000 ISK - 7.5% sales tax = 4,625 ISK) and wallet journal entries
    let s2_bal_after_fill = ctx.db.get_character(90_000_002).unwrap().balance;
    let seller_net_isk = (s2_bal_after_fill.0 - s2_bal_before_fill.0) as f64 / 100.0;
    assert!(
        (seller_net_isk - 4_625.0).abs() < 0.02,
        "Seller must receive gross minus sales tax (4,625.00 ISK), got {seller_net_isk}"
    );
    ctx.db.with_snapshot(|snap| {
        assert!(
            snap.wallet_journal
                .iter()
                .any(|j| j.character_id == 90_000_001 && j.entry_type == "MarketTransaction"),
            "Buyer MarketTransaction journal entry must be recorded"
        );
        assert!(
            snap.wallet_journal
                .iter()
                .any(|j| j.character_id == 90_000_002 && j.entry_type == "MarketSell"),
            "Seller MarketSell journal entry must be recorded"
        );
    });

    // Exercise V24 BuyMultipleItems(stationID, itemList) sweeping across the remaining 500 units @ 10.00 ISK
    // and 300 units @ 12.00 ISK (total 800 units, total cost = 500*10 + 300*12 = 8,600.00 ISK)
    let multi_buy = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
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
            &kw,
            None,
        )
        .await
        .expect("BuyMultipleItems");
    let PyRep::KeyVal(mb_kv) = multi_buy else {
        panic!("Expected KeyVal from BuyMultipleItems");
    };
    assert_eq!(mb_kv.get("purchasedCount"), Some(&PyRep::Integer(1)));
    assert_eq!(mb_kv.get("totalQuantity"), Some(&PyRep::Integer(800)));
    assert_eq!(mb_kv.get("totalCostISK"), Some(&PyRep::Real(8_600.0)));
    assert!(matches!(mb_kv.get("ordersCreated"), Some(PyRep::List(l)) if l.len() == 2));

    // Verify buyer received 500 + 800 = 1,300 Tritanium in hangar
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
    assert!(s1_hangar_trit >= 1_300, "Buyer must have received purchased Tritanium");

    // 2. Instant Sell (feat:market.instant-sell, feat:market.place-buy)
    // Pilot 1 places resting regional buy order (orderRange=32767, minVolume=300) at 1.00 ISK for 400 units via PlaceBuyOrder
    let resting_bid = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(1.0),
                PyRep::Integer(400),
                PyRep::Integer(32767), // regional range
                PyRep::Integer(300),   // minVolume = 300
                PyRep::Integer(90),    // duration = 90 days
                PyRep::Bool(false),
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

    // Attempting an immediate sell (duration = 0) of 200 units (< minVolume 300) must not match the resting bid
    let under_min_err = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
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
            &kw,
            None,
        )
        .await;
    assert_eq!(
        under_min_err.unwrap_err().code,
        "ImmediateOrderUnfilled",
        "Sell quantity below resting bid minVolume must not match"
    );

    let s2_bal_before_isell = ctx.db.get_character(90_000_002).unwrap().balance;

    // Pilot 2 places sell order at 1.00 ISK for 400 units (>= minVolume 300) via PlaceMultiSellOrder, immediately crossing the resting bid
    let instant_sell = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
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

    // Verify invalid contract type (e.g., 4 = Loan) is rejected
    let invalid_type_err = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(4), // Loan (unsupported)
                PyRep::Integer(0), // avail = public
                PyRep::Integer(0), // assigneeID
                PyRep::Integer(1440),
                PyRep::Integer(0),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Real(10_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
                PyRep::String("Invalid Loan".into()),
                PyRep::String("Should fail".into()),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(invalid_type_err.code, "InvalidContractType");

    // 1. One-way ItemExchange Contract using V24 16-argument CreateContract signature (feat:contracts.item-exchange)
    // Pilot 1 sells item 100_000_003 (1MN Afterburner II) for 75,000 ISK
    let c_exchange = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(1),               // conType = CONTYPE_AUCTIONANDITEMECHANGE (1)
                PyRep::Integer(0),               // avail = public (0)
                PyRep::Integer(0),               // assigneeID
                PyRep::Integer(20160),           // expireTime (minutes, 14 days)
                PyRep::Integer(0),               // duration
                PyRep::Integer(JITA_STATION_ID), // startStationID
                PyRep::Integer(JITA_STATION_ID), // endStationID
                PyRep::Real(75_000.0),           // price
                PyRep::Real(0.0),                // reward
                PyRep::Real(0.0),                // collateral
                PyRep::String("Afterburner Sale".into()),
                PyRep::String("1MN Afterburner II in Jita".into()),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(100_000_003),
                    PyRep::Integer(1),
                ])]),                            // itemList
                PyRep::Integer(4),               // flag = flagHangar (4)
                PyRep::List(vec![]),             // requestItemTypeList
                PyRep::Bool(false),              // forCorp
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
    // Pilot 1 creates a private contract to Pilot 2 paying 10,000 ISK reward, requesting 500 units of Tritanium (type 34)
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
                PyRep::Integer(1),               // conType = 1 (ItemExchange)
                PyRep::Integer(1),               // avail = 1 (Private)
                PyRep::Integer(90_000_002),      // assigneeID = pilot 2
                PyRep::Integer(20160),           // expireTime
                PyRep::Integer(0),               // duration
                PyRep::Integer(JITA_STATION_ID), // startStationID
                PyRep::Integer(JITA_STATION_ID), // endStationID
                PyRep::Real(0.0),                // price
                PyRep::Real(10_000.0),           // reward paid to acceptor
                PyRep::Real(0.0),                // collateral
                PyRep::String("WTB Tritanium".into()),
                PyRep::String("Need 500 Tritanium".into()),
                PyRep::List(vec![]),             // itemList (offering no items)
                PyRep::Integer(4),               // flag = flagHangar (4)
                requested_items,                 // requestItemTypeList
                PyRep::Bool(false),              // forCorp
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

    // Attempting to accept before Pilot 2 has the requested 500 Tritanium must fail with MissingRequestedContractItems
    let missing_req_err = ctx
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
        .unwrap_err();
    assert_eq!(
        missing_req_err.code, "MissingRequestedContractItems",
        "Accepting an ItemExchange contract without the requested items in hangar must be refused"
    );

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

    // Pilot 2 accepts the request contract once the requested Tritanium is in hangar
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

    // Create 3rd pilot for auction outbidding and items for auction + courier
    let (auc_item_id, auc2_item_id, cour_item_id) = ctx.db.with_mut_snapshot(|snap| {
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
        snap.next_entity_id += 1;
        let auc2_item_id = snap.next_entity_id;
        snap.items.insert(
            auc2_item_id,
            InventoryItemRecord {
                item_id: auc2_item_id,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let cour_item_id = snap.next_entity_id;
        snap.items.insert(
            cour_item_id,
            InventoryItemRecord {
                item_id: cour_item_id,
                type_id: 35, // Pyerite
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 250,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        (auc_item_id, auc2_item_id, cour_item_id)
    });
    let mut s3 = setup_pilot(&ctx, 305, "third", "third-secret", 90_000_003).await;

    // 1. Auction Contract using V24 16-arg CreateContract (conType = 2) (feat:contracts.auction)
    // Pilot 1 creates Auction: start_bid = 100,000 ISK, buyout = 500,000 ISK
    let c_auction = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(2),               // conType = 2 (Auction)
                PyRep::Integer(0),               // avail = public
                PyRep::Integer(0),               // assigneeID
                PyRep::Integer(20160),           // expireTime (minutes)
                PyRep::Integer(0),               // duration
                PyRep::Integer(JITA_STATION_ID), // startStationID
                PyRep::Integer(JITA_STATION_ID), // endStationID
                PyRep::Real(100_000.0),          // starting bid
                PyRep::Real(0.0),                // reward (unused for Auction)
                PyRep::Real(500_000.0),          // buyout
                PyRep::String("Rifter Auction".into()),
                PyRep::String("Hull auction".into()),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(auc_item_id),
                    PyRep::Integer(1),
                ])]),
                PyRep::Integer(4),
                PyRep::List(vec![]),
                PyRep::Bool(false),
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

    // Bid below minimum increment (5% of 150,000 = 7,500 ISK -> min next bid 157,500 ISK) must be rejected with BidTooLow
    let low_bid_err = ctx
        .dispatch_rpc(
            &mut s3,
            "contractProxy",
            None,
            "PlaceBid",
            &[PyRep::Integer(auc_cid), PyRep::Real(151_000.0)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        low_bid_err.code, "BidTooLow",
        "Bid below minimum increment must be rejected with BidTooLow"
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

    // Verify GetMyBids returns active bids for Pilot 3
    let my_bids = ctx
        .dispatch_rpc(
            &mut s3,
            "contractProxy",
            None,
            "GetMyBids",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetMyBids");
    assert!(matches!(my_bids, PyRep::List(ref l) if !l.is_empty()));

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

    // Also verify auction expiry settlement to highest bidder when auction expires without buyout
    let c_auction2 = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(2),               // conType = 2 (Auction)
                PyRep::Integer(0),               // avail = public
                PyRep::Integer(0),               // assigneeID
                PyRep::Integer(20160),           // expireTime
                PyRep::Integer(0),               // duration
                PyRep::Integer(JITA_STATION_ID), // startStationID
                PyRep::Integer(JITA_STATION_ID), // endStationID
                PyRep::Real(100_000.0),          // starting bid
                PyRep::Real(0.0),
                PyRep::Real(0.0),                // no buyout
                PyRep::String("No-Buyout Rifter Auction".into()),
                PyRep::String("Settles at expiry".into()),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(auc2_item_id),
                    PyRep::Integer(1),
                ])]),
                PyRep::Integer(4),
                PyRep::List(vec![]),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("Create 2nd Auction Contract");
    let PyRep::KeyVal(auc2_kv) = c_auction2 else {
        panic!("Expected KeyVal");
    };
    let auc2_cid = auc2_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(
        &mut s2,
        "contractProxy",
        None,
        "PlaceBid",
        &[PyRep::Integer(auc2_cid), PyRep::Real(120_000.0)],
        &kw,
        None,
    )
    .await
    .expect("PlaceBid on 2nd auction");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.contracts.get_mut(&auc2_cid) {
            c.expiration_unix = 1;
        }
    });
    let settled_auc = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "GetContract",
            &[PyRep::Integer(auc2_cid)],
            &kw,
            None,
        )
        .await
        .expect("GetContract settles expired auction");
    let PyRep::KeyVal(sauc_kv) = settled_auc else {
        panic!("Expected KeyVal");
    };
    assert_eq!(sauc_kv.get("status"), Some(&PyRep::String("Completed".into())));
    let item_owner_auc2 = ctx.db.with_snapshot(|snap| {
        snap.items.get(&auc2_item_id).map(|i| (i.owner_id, i.flag))
    });
    assert_eq!(
        item_owner_auc2,
        Some((90_000_002, InventoryFlag::Hangar)),
        "Expired auction with winning bid must deliver item to highest bidder"
    );

    // 2. Courier Contract using V24 16-arg CreateContract (conType = 3) (feat:contracts.courier)
    // Start at Jita 4-4 (60003760), deliver to Jita 4-4 CBT Bureau Offices (60003766), reward 50k, collateral 200k,
    // with listing expireTime = 20160 min (14 days = 1,209,600s) != courier duration = 3 days (259,200s)
    let dest_station_id = 60_003_766_i64;
    let create_ts = ctx.now_unix() as i64;
    let c_courier = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(3),               // conType = 3 (Courier)
                PyRep::Integer(0),               // avail = public
                PyRep::Integer(0),               // assigneeID
                PyRep::Integer(20160),           // expireTime (14 days in minutes)
                PyRep::Integer(3),               // duration (3 days to complete after acceptance)
                PyRep::Integer(JITA_STATION_ID), // startStationID
                PyRep::Integer(dest_station_id), // endStationID
                PyRep::Real(0.0),                // price
                PyRep::Real(50_000.0),           // reward
                PyRep::Real(200_000.0),          // collateral
                PyRep::String("Haul Pyerite".into()),
                PyRep::String("Deliver to 60003766".into()),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(cour_item_id),
                    PyRep::Integer(250),
                ])]),
                PyRep::Integer(4),
                PyRep::List(vec![]),
                PyRep::Bool(false),
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
    let listing_exp = cour_kv.get("expirationUnix").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(cour_kv.get("numDays"), Some(&PyRep::Integer(3)));
    assert_eq!(
        listing_exp,
        create_ts + 20_160 * 60,
        "CreateContract must use expireTime (args[3], 20160 min = 14 days) as listing expiry"
    );

    let s2_bal_before_courier = ctx.db.get_character(90_000_002).unwrap().balance;

    // Pilot 2 accepts the courier contract -> collateral debited, items wrapped in Plastic Wrap (3468), and delivery deadline set to now + 3 * 86,400s
    let s2_ship_id = s2.ship_id.unwrap();
    let accept_ts = ctx.now_unix() as i64;
    let accept_cour = ctx
        .dispatch_rpc(
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
    let PyRep::KeyVal(acc_cour_kv) = accept_cour else {
        panic!("Expected KeyVal from AcceptContract");
    };
    let delivery_exp = acc_cour_kv
        .get("expirationUnix")
        .and_then(|v| v.as_i64())
        .unwrap();
    assert_eq!(
        delivery_exp,
        accept_ts + 3 * 86_400,
        "AcceptContract for a courier must set delivery deadline to now + duration_days * 86400 (3 days = 259,200s)"
    );
    let wrap_id = acc_cour_kv
        .get("courierPackageID")
        .and_then(|v| v.as_i64())
        .expect("AcceptContract for courier must return courierPackageID");
    let wrap_item = ctx.db.get_item(wrap_id).expect("Plastic Wrap item must exist");
    assert_eq!(wrap_item.type_id, 3468, "Courier package must be SDE Plastic Wrap (type_id 3468)");
    assert_eq!(wrap_item.owner_id, 90_000_002);
    assert_eq!(wrap_item.location_id, s2_ship_id);
    assert_eq!(wrap_item.flag, InventoryFlag::Cargo);
    assert_eq!(
        ctx.db.get_item(cour_item_id).unwrap().location_id,
        wrap_id,
        "Contracted courier item must be placed inside the Plastic Wrap container"
    );

    let s2_bal_after_accept_cour = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(
        s2_bal_before_courier.0 - s2_bal_after_accept_cour.0,
        20_000_000,
        "Acceptor must post collateral for courier contract"
    );

    // Attempting to complete before reaching dest_station_id must fail with CourierNotAtDestinationStation
    let wrong_st_err = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "CompleteContract",
            &[PyRep::Integer(cour_cid)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(wrong_st_err.code, "CourierNotAtDestinationStation");

    // If Pilot 2 and active ship dock at destination station 60003766 without the Plastic Wrap package at the destination, CompleteContract must fail with CourierPackageMissing
    s2.station_id = Some(dest_station_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.station_id = Some(dest_station_id);
        }
        if let Some(sh) = snap.items.get_mut(&s2_ship_id) {
            sh.location_id = dest_station_id;
        }
        if let Some(wrap) = snap.items.get_mut(&wrap_id) {
            wrap.location_id = JITA_STATION_ID;
        }
    });
    let missing_pkg_err = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "CompleteContract",
            &[PyRep::Integer(cour_cid)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(missing_pkg_err.code, "CourierPackageMissing");

    // Put the Plastic Wrap back into Pilot 2's active ship at destination station 60003766 and complete contract
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(wrap) = snap.items.get_mut(&wrap_id) {
            wrap.location_id = s2_ship_id;
        }
    });

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
    .expect("Complete Courier Contract at destination station");

    let s2_bal_after_complete_cour = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(
        s2_bal_after_complete_cour.0 - s2_bal_before_courier.0,
        5_000_000,
        "Collateral refunded and reward (50,000 ISK) paid to courier runner"
    );
    assert!(
        ctx.db.get_item(wrap_id).is_none(),
        "Plastic Wrap container must be unwrapped and removed upon courier completion"
    );
    let delivered_cour_item = ctx.db.with_snapshot(|snap| {
        snap.items
            .get(&cour_item_id)
            .map(|i| (i.owner_id, i.location_id, i.flag, i.quantity))
    });
    assert_eq!(
        delivered_cour_item,
        Some((90_000_001, dest_station_id, InventoryFlag::Hangar, 250)),
        "Completed courier contract must deliver items to issuer's hangar at destination station"
    );

    // Return Pilot 2 and ship to Jita 4-4
    s2.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.station_id = Some(JITA_STATION_ID);
        }
        if let Some(sh) = snap.items.get_mut(&s2_ship_id) {
            sh.location_id = JITA_STATION_ID;
        }
    });

    // 3. Courier Contract Failure & Deadline Expiry (feat:contracts.courier-failure)
    let empty_cour_err = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(3),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(20160),
                PyRep::Integer(1),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(dest_station_id),
                PyRep::Real(0.0),
                PyRep::Real(50_000.0),
                PyRep::Real(300_000.0),
                PyRep::String("Empty Courier".into()),
                PyRep::String("Should fail".into()),
                PyRep::List(vec![]),
                PyRep::Integer(4),
                PyRep::List(vec![]),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(empty_cour_err.code, "CourierContractRequiresItems");

    let (fail_item_id, exp_item_id) = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let i1 = snap.next_entity_id;
        snap.items.insert(
            i1,
            InventoryItemRecord {
                item_id: i1,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
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
            InventoryItemRecord {
                item_id: i2,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        (i1, i2)
    });

    let c_fail_courier = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(3),               // conType = 3 (Courier)
                PyRep::Integer(0),               // avail = public
                PyRep::Integer(0),               // assigneeID
                PyRep::Integer(20160),           // expireTime
                PyRep::Integer(1),               // duration
                PyRep::Integer(JITA_STATION_ID), // startStationID
                PyRep::Integer(dest_station_id), // endStationID
                PyRep::Real(0.0),                // price
                PyRep::Real(50_000.0),           // reward
                PyRep::Real(300_000.0),          // collateral
                PyRep::String("Courier To Fail".into()),
                PyRep::String("Failure test".into()),
                PyRep::List(vec![PyRep::Integer(fail_item_id)]),
                PyRep::Integer(4),
                PyRep::List(vec![]),
                PyRep::Bool(false),
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

    // Also verify courier contract exceeding delivery deadline automatically fails and forfeits collateral to issuer
    let c_exp_courier = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(3),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(20160),
                PyRep::Integer(1),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(dest_station_id),
                PyRep::Real(0.0),
                PyRep::Real(25_000.0),
                PyRep::Real(100_000.0),
                PyRep::String("Overdue Courier".into()),
                PyRep::String("Deadline expiry test".into()),
                PyRep::List(vec![PyRep::Integer(exp_item_id)]),
                PyRep::Integer(4),
                PyRep::List(vec![]),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("Create Courier Contract for deadline expiry");
    let PyRep::KeyVal(cexp_kv) = c_exp_courier else {
        panic!("Expected KeyVal");
    };
    let exp_cour_cid = cexp_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(
        &mut s2,
        "contractProxy",
        None,
        "AcceptContract",
        &[PyRep::Integer(exp_cour_cid)],
        &kw,
        None,
    )
    .await
    .expect("Accept overdue courier");
    let s1_bal_before_overdue = ctx.db.get_character(90_000_001).unwrap().balance;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.contracts.get_mut(&exp_cour_cid) {
            c.expiration_unix = 1; // delivery deadline elapsed
        }
    });
    let overdue_err = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "CompleteContract",
            &[PyRep::Integer(exp_cour_cid)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(overdue_err.code, "ContractExpired");
    let s1_bal_after_overdue = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(
        s1_bal_after_overdue.0 - s1_bal_before_overdue.0,
        12_500_000, // 100,000 collateral + 25,000 reward refund
        "Overdue courier contract must forfeit collateral and refund reward to issuer"
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

    // Seed a Rifter Blueprint item and an Upwell Raitaru Engineering Complex (type 35825) in Jita
    let (rifter_bpo_id, raitaru_id) = ctx.db.with_mut_snapshot(|snap| {
        let corp_id = snap.characters[&90_000_001].corporation_id;
        snap.next_entity_id += 1;
        let bpo_id = snap.next_entity_id;
        snap.items.insert(
            bpo_id,
            InventoryItemRecord {
                item_id: bpo_id,
                type_id: RIFTER_BP_TYPE_ID,
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let struct_id = snap.next_entity_id;
        snap.items.insert(
            struct_id,
            InventoryItemRecord {
                item_id: struct_id,
                type_id: 35825, // Raitaru Medium Engineering Complex
                owner_id: corp_id,
                location_id: 30_000_142, // Jita solar system
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Set Mass Production (3387) and Advanced Mass Production (24625) to level 0 -> 1 manufacturing slot
        snap.skills.entry(90_000_001).or_default().remove(&3387);
        snap.skills.entry(90_000_001).or_default().remove(&24625);
        (bpo_id, struct_id)
    });

    // 1. Blueprint data inspection via V24 blueprintManager (feat:industry.blueprint-data)
    let bp_data = ctx
        .dispatch_rpc(
            &mut s1,
            "blueprintManager",
            None,
            "GetBlueprintData",
            &[PyRep::Integer(rifter_bpo_id)],
            &kw,
            None,
        )
        .await
        .expect("blueprintManager.GetBlueprintData");

    let PyRep::KeyVal(bp_kv) = bp_data else {
        panic!("Expected KeyVal from blueprintManager.GetBlueprintData");
    };
    assert_eq!(bp_kv.get("blueprintID"), Some(&PyRep::Integer(rifter_bpo_id)));
    assert_eq!(bp_kv.get("blueprintTypeID"), Some(&PyRep::Integer(RIFTER_BP_TYPE_ID as i64)));
    assert_eq!(bp_kv.get("productTypeID"), Some(&PyRep::Integer(587))); // Rifter ship
    assert!(bp_kv.get("manufacturingTime").and_then(|v| v.as_i64()).unwrap() > 0);
    assert!(
        matches!(bp_kv.get("materials"), Some(PyRep::List(m)) if !m.is_empty()),
        "GetBlueprintData must return required input materials list"
    );
    assert!(
        matches!(bp_kv.get("skills"), Some(PyRep::List(_))),
        "GetBlueprintData must return required skills list"
    );

    let owner_bps = ctx
        .dispatch_rpc(
            &mut s1,
            "blueprintManager",
            None,
            "GetBlueprintDataByOwner",
            &[PyRep::Integer(90_000_001), PyRep::Integer(JITA_STATION_ID)],
            &kw,
            None,
        )
        .await
        .expect("blueprintManager.GetBlueprintDataByOwner");
    let PyRep::List(owner_bp_list) = owner_bps else {
        panic!("Expected List from GetBlueprintDataByOwner");
    };
    assert!(!owner_bp_list.is_empty());

    let limits_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "blueprintManager",
            None,
            "GetLimits",
            &[],
            &kw,
            None,
        )
        .await
        .expect("blueprintManager.GetLimits");
    let PyRep::KeyVal(limits_kv) = limits_rep else {
        panic!("Expected KeyVal from blueprintManager.GetLimits");
    };
    assert!(!limits_kv.is_empty());

    // 2. Facilities query & Upwell structure modifiers via V24 facilityManager (feat:industry.facilities)
    let facilities_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "facilityManager",
            None,
            "GetFacilities",
            &[],
            &kw,
            None,
        )
        .await
        .expect("facilityManager.GetFacilities");

    let PyRep::List(fac_list) = facilities_rep else {
        panic!("Expected List from facilityManager.GetFacilities");
    };
    assert!(fac_list.len() >= 2, "Must return NPC stations and Upwell Raitaru facility");

    let raitaru_fac = ctx
        .dispatch_rpc(
            &mut s1,
            "facilityManager",
            None,
            "GetFacility",
            &[PyRep::Integer(raitaru_id)],
            &kw,
            None,
        )
        .await
        .expect("facilityManager.GetFacility Raitaru");
    let PyRep::KeyVal(raitaru_kv) = raitaru_fac else {
        panic!("Expected KeyVal from facilityManager.GetFacility");
    };
    assert_eq!(raitaru_kv.get("facilityID"), Some(&PyRep::Integer(raitaru_id)));
    assert_eq!(raitaru_kv.get("typeID"), Some(&PyRep::Integer(35825)));
    assert_eq!(raitaru_kv.get("costModifier"), Some(&PyRep::Real(0.97)));
    assert_eq!(raitaru_kv.get("materialModifier"), Some(&PyRep::Real(0.99)));
    assert_eq!(raitaru_kv.get("timeModifier"), Some(&PyRep::Real(0.85)));

    let max_mods = ctx
        .dispatch_rpc(
            &mut s1,
            "facilityManager",
            None,
            "GetMaxActivityModifiers",
            &[],
            &kw,
            None,
        )
        .await
        .expect("facilityManager.GetMaxActivityModifiers");
    assert!(matches!(max_mods, PyRep::KeyVal(_)));

    // 3. Facility tax rate configuration via V24 facilityManager (feat:industry.facility-tax)
    // Negative tax rate must be rejected with InvalidFacilityTaxRate
    let neg_tax_err = ctx
        .dispatch_rpc(
            &mut s1,
            "facilityManager",
            None,
            "SetFacilityTaxes",
            &[
                PyRep::Integer(raitaru_id),
                PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(-0.02))]),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(neg_tax_err.code, "InvalidFacilityTaxRate");

    // Set Raitaru manufacturing (activity 1) tax rate to 5.0% (0.05)
    ctx.dispatch_rpc(
        &mut s1,
        "facilityManager",
        None,
        "SetFacilityTaxes",
        &[
            PyRep::Integer(raitaru_id),
            PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.05))]),
        ],
        &kw,
        None,
    )
    .await
    .expect("facilityManager.SetFacilityTaxes");

    let taxes_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "facilityManager",
            None,
            "GetFacilityTaxes",
            &[PyRep::Integer(raitaru_id), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("facilityManager.GetFacilityTaxes");
    let PyRep::KeyVal(taxes_kv) = taxes_rep else {
        panic!("Expected KeyVal from GetFacilityTaxes");
    };
    let mfg_tax_rate = taxes_kv
        .get("1")
        .or_else(|| taxes_kv.get("manufacturing"))
        .and_then(|v| v.as_f64())
        .expect("Activity 1 tax rate");
    assert!((mfg_tax_rate - 0.05).abs() < 1e-6);

    // 4. Job cost calculation & installation via V24 industryManager.InstallJob (feat:industry.job-cost, feat:industry.install-job)
    let s1_bal_before_job = ctx.db.get_character(90_000_001).unwrap().balance;

    // Install 2-run Manufacturing job on the Raitaru facility using V24 job KeyVal
    let job_arg = PyRep::keyval([
        ("blueprintID", PyRep::Integer(rifter_bpo_id)),
        ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
        ("facilityID", PyRep::Integer(raitaru_id)),
        ("solarSystemID", PyRep::Integer(30_000_142)),
        ("activityID", PyRep::Integer(1)), // Manufacturing
        ("runs", PyRep::Integer(2)),
        ("productTypeID", PyRep::Integer(587)),
        ("licensedRuns", PyRep::Integer(2)),
        ("status", PyRep::Integer(1)), // STATUS_INSTALLED
    ]);

    let mfg_install = ctx
        .dispatch_rpc(
            &mut s1,
            "industryManager",
            None,
            "InstallJob",
            &[job_arg],
            &kw,
            None,
        )
        .await
        .expect("industryManager.InstallJob V24");

    let PyRep::KeyVal(mfg_kv) = mfg_install else {
        panic!("Expected KeyVal from InstallJob");
    };
    let mfg_job_id = mfg_kv.get("jobID").and_then(|v| v.as_i64()).unwrap();
    let eiv = mfg_kv.get("estimatedItemValue").and_then(|v| v.as_f64()).unwrap();
    let sci = mfg_kv.get("systemCostIndex").and_then(|v| v.as_f64()).unwrap();
    let struct_mod = mfg_kv.get("structureCostModifier").and_then(|v| v.as_f64()).unwrap();
    let base_job_cost = mfg_kv.get("baseJobCost").and_then(|v| v.as_f64()).unwrap();
    let fac_tax_isk = mfg_kv.get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    let scc_isk = mfg_kv.get("sccSurchargeISK").and_then(|v| v.as_f64()).unwrap();
    let total_cost_isk = mfg_kv.get("totalJobCost").and_then(|v| v.as_f64()).unwrap();

    // Verify EIV + System Cost Index + Raitaru -3% cost modifier + 5% custom facility tax + 4% SCC surcharge
    assert!(eiv > 0.0, "EIV must be computed from blueprint input materials");
    assert!(sci > 0.0 && sci <= 0.50, "Jita manufacturing system cost index must be positive");
    assert!((struct_mod - 0.97).abs() < 1e-6, "Raitaru cost modifier must be 0.97");
    let expected_base = (eiv * sci * 0.97 * 2.0 * 100.0).round() / 100.0;
    let expected_fac_tax = (expected_base * 0.05 * 100.0).round() / 100.0;
    let expected_scc = (expected_base * 0.04 * 100.0).round() / 100.0;
    let expected_total = ((expected_base + expected_fac_tax + expected_scc) * 100.0).round() / 100.0;
    assert!((base_job_cost - expected_base).abs() < 0.02);
    assert!((fac_tax_isk - expected_fac_tax).abs() < 0.02);
    assert!((scc_isk - expected_scc).abs() < 0.02);
    assert!((total_cost_isk - expected_total).abs() < 0.02);

    // Verify total job cost was debited from wallet balance and journal entries were recorded
    let s1_bal_after_job = ctx.db.get_character(90_000_001).unwrap().balance;
    let debited_isk = (s1_bal_before_job.0 - s1_bal_after_job.0) as f64 / 100.0;
    assert!(
        (debited_isk - total_cost_isk).abs() < 0.02,
        "Wallet debit ({debited_isk}) must match totalJobCost ({total_cost_isk})"
    );
    let (has_job_tax, has_fac_tax) = ctx.db.with_snapshot(|snap| {
        let jt = snap.wallet_journal.iter().any(|j| {
            j.character_id == 90_000_001
                && (j.entry_type == "IndustryJobInstallationFee"
                    || j.entry_type == "IndustryJobTax")
        });
        let ft = snap.wallet_journal.iter().any(|j| {
            j.character_id == 90_000_001 && j.entry_type == "IndustryFacilityTax"
        });
        (jt, ft)
    });
    assert!(has_job_tax && has_fac_tax, "Wallet journal must record IndustryJobInstallationFee and IndustryFacilityTax");

    // Verify slot limit enforcement: pilot has 1 manufacturing slot (Mass Production = 0), so a 2nd manufacturing job fails with IndustrySlotsFull
    let slot_err = ctx
        .dispatch_rpc(
            &mut s1,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(0)),
                ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
                ("facilityID", PyRep::Integer(JITA_STATION_ID)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(slot_err.code, "IndustrySlotsFull");

    // Verify GetJobCounts and GetJob
    let counts_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "industryManager",
            None,
            "GetJobCounts",
            &[PyRep::Integer(90_000_001)],
            &kw,
            None,
        )
        .await
        .expect("industryManager.GetJobCounts");
    assert!(matches!(counts_rep, PyRep::KeyVal(_)));

    let job_detail = ctx
        .dispatch_rpc(
            &mut s1,
            "industryManager",
            None,
            "GetJob",
            &[PyRep::Integer(mfg_job_id)],
            &kw,
            None,
        )
        .await
        .expect("industryManager.GetJob");
    let PyRep::KeyVal(jd_kv) = job_detail else {
        panic!("Expected KeyVal from GetJob");
    };
    assert_eq!(jd_kv.get("jobID"), Some(&PyRep::Integer(mfg_job_id)));

    // Fast-forward duration and complete job via CompleteJob
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
        .expect("industryManager.CompleteJob");

    let PyRep::KeyVal(cres_kv) = complete_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(cres_kv.get("productTypeID"), Some(&PyRep::Integer(587))); // Rifter
    let delivered_id = cres_kv.get("deliveredItemID").and_then(|v| v.as_i64()).unwrap();

    let delivered_item = ctx.db.with_snapshot(|snap| {
        snap.items.get(&delivered_id).map(|i| (i.owner_id, i.location_id, i.flag, i.quantity))
    });
    assert_eq!(
        delivered_item,
        Some((90_000_001, JITA_STATION_ID, InventoryFlag::Hangar, 2)),
        "2 runs of Rifter manufacturing must deliver 2 Rifters to hangar"
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

    // 1. Planet info & resource scanning (feat:pi.planet-info)
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
    assert!(pkv.get("planetTypeID").and_then(|v| v.as_i64()).unwrap() > 0);
    assert!(pkv.get("radius").and_then(|v| v.as_f64()).unwrap() > 0.0);

    let res_info = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "GetPlanetResourceInfo",
            &[PyRep::Integer(JITA_PLANET_ID)],
            &kw,
            None,
        )
        .await
        .expect("GetPlanetResourceInfo");
    let PyRep::KeyVal(res_kv) = res_info else {
        panic!("Expected KeyVal from GetPlanetResourceInfo");
    };
    let Some(PyRep::List(res_list)) = res_kv.get("resources") else {
        panic!("Expected resources List in GetPlanetResourceInfo");
    };
    assert!(!res_list.is_empty(), "GetPlanetResourceInfo must return resource types for planet");
    let first_res_type_id = match &res_list[0] {
        PyRep::KeyVal(kv) => kv.get("resourceTypeID").and_then(|v| v.as_i64()).unwrap(),
        _ => panic!("Expected KeyVal entry in resources"),
    };

    // Verify GetResourceData requires Remote Sensing (13279) >= 1
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().remove(&13279);
    });
    let scan_err = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "GetResourceData",
            &[PyRep::keyval([
                ("planetID", PyRep::Integer(JITA_PLANET_ID)),
                ("resourceTypeID", PyRep::Integer(first_res_type_id)),
            ])],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(scan_err.code, "PISkillRequiredToSurveyPlanetResources");

    // Train Remote Sensing (13279) = 3, Planetology (2406) = 4, Advanced Planetology (2403) = 2
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(90_000_001).or_default();
        sk.insert(13279, TrainedSkillRecord { type_id: 13279, level: 3, skill_points: 45_255.0 });
        sk.insert(2406, TrainedSkillRecord { type_id: 2406, level: 4, skill_points: 135_765.0 });
        sk.insert(2403, TrainedSkillRecord { type_id: 2403, level: 2, skill_points: 7_072.0 });
    });

    let res_data = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "GetResourceData",
            &[PyRep::keyval([
                ("planetID", PyRep::Integer(JITA_PLANET_ID)),
                ("resourceTypeID", PyRep::Integer(first_res_type_id)),
                ("advancedPlanetology", PyRep::Integer(2)),
                ("planetology", PyRep::Integer(4)),
                ("remoteSensing", PyRep::Integer(3)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("GetResourceData");
    let PyRep::KeyVal(rd_kv) = res_data else {
        panic!("Expected KeyVal from GetResourceData");
    };
    // numBands = 2 + 4 + 2*2 = 10
    assert_eq!(rd_kv.get("numBands"), Some(&PyRep::Integer(10)));
    assert_eq!(rd_kv.get("proximity"), Some(&PyRep::Integer(5)));
    assert!(matches!(rd_kv.get("data"), Some(PyRep::Bytes(b)) if !b.is_empty()));

    // 2. Command center placement via V24 UserUpdateNetwork COMMAND_CREATEPIN = 1 (feat:pi.command-center)
    // Attempting to place a Command Center while docked at a station must fail with CannotPlaceCommandCenterWhileDocked
    let docked_cc_err = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(1), // COMMAND_CREATEPIN
                    PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(1)]),
                    PyRep::Integer(2534), // Gas Command Center
                    PyRep::Real(0.35),
                    PyRep::Real(-1.10),
                ])]),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        docked_cc_err.code, "CannotPlaceCommandCenterWhileDocked",
        "Deploying a Command Center while docked must be rejected"
    );

    // Undock s1 into Jita space; placing a Command Center without one in the active ship's cargo hold must fail with CommandCenterItemRequired
    let s1_ship_id = s1.ship_id.expect("s1 active ship");
    s1.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
        }
    });
    let missing_cc_err = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(1), // COMMAND_CREATEPIN
                    PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(1)]),
                    PyRep::Integer(2534), // Gas Command Center
                    PyRep::Real(0.35),
                    PyRep::Real(-1.10),
                ])]),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(missing_cc_err.code, "CommandCenterItemRequired");

    // R69-P1-2: Attempting to place a Temperate Command Center (2254, planetRestriction = 11) on Jita IV (40009082, Planet (Gas) = 13) must fail with IncompatibleCommandCenterPlanetType
    let mismatch_cc_err = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(1), // COMMAND_CREATEPIN
                    PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(1)]),
                    PyRep::Integer(2254), // Temperate Command Center on Gas planet
                    PyRep::Real(0.35),
                    PyRep::Real(-1.10),
                ])]),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        mismatch_cc_err.code, "IncompatibleCommandCenterPlanetType",
        "Deploying a Temperate Command Center (2254) on a Gas planet (40009082) must be rejected"
    );

    // Seed 2x Gas Command Center (2534) in pilot's active ship cargo hold while undocked in space
    let cc_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 2534,
                owner_id: 90_000_001,
                location_id: s1_ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        id
    });

    let create_col = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(1), // COMMAND_CREATEPIN
                    PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(1)]), // (0, 1) command center pin ID
                    PyRep::Integer(2534), // Gas Command Center
                    PyRep::Real(0.35),    // latitude (radians)
                    PyRep::Real(-1.10),   // longitude (radians)
                ])]),
            ],
            &kw,
            None,
        )
        .await
        .expect("UserUpdateNetwork place Command Center");

    let PyRep::KeyVal(col_kv) = create_col else {
        panic!("Expected KeyVal from UserUpdateNetwork");
    };
    assert_eq!(col_kv.get("currentLevel"), Some(&PyRep::Integer(0)));
    assert_eq!(col_kv.get("commandCenterLevel"), Some(&PyRep::Integer(0)));
    assert_eq!(col_kv.get("cpuCapacity"), Some(&PyRep::Integer(1675)));
    assert_eq!(col_kv.get("powerCapacity"), Some(&PyRep::Integer(6000)));
    assert_eq!(col_kv.get("pinsCount"), Some(&PyRep::Integer(1)));
    assert_eq!(
        ctx.db.get_item(cc_item_id).unwrap().quantity,
        1,
        "Placing a Command Center must consume 1 Command Center item from active ship cargo"
    );
    let Some(PyRep::List(cc_pins)) = col_kv.get("pins") else {
        panic!("Expected pins list in UserUpdateNetwork response");
    };
    let PyRep::KeyVal(cc_pin_kv) = &cc_pins[0] else {
        panic!("Expected KeyVal for Command Center pin");
    };
    let cc_pin_id = cc_pin_kv.get("pinID").and_then(|v| v.as_i64()).unwrap();
    assert!(cc_pin_id > 1, "Server must allocate its own positive pinID");

    // 3. Colony limit enforcement (feat:pi.colony-limit)
    // Pilot has Interplanetary Consolidation (2495) level 0 -> limit is 1 colony
    // Attempting to place a Command Center on a second planet (Jita V = 40009098, Gas) must fail with MaxColoniesReached
    let second_planet_id = 40_009_098; // Jita V (Planet (Gas))
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
            "UserUpdateNetwork",
            &[
                PyRep::Integer(second_planet_id),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(1), // COMMAND_CREATEPIN
                    PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(1)]),
                    PyRep::Integer(2534),
                    PyRep::Real(0.10),
                    PyRep::Real(0.20),
                ])]),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        col_limit_err.code, "MaxColoniesReached",
        "Exceeding 1 + Interplanetary Consolidation limit must return MaxColoniesReached"
    );

    // Re-dock s1 at Jita 4-4; existing colonies can be upgraded and managed while docked or in space
    s1.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(JITA_STATION_ID);
        }
    });

    // 4. Command Center Upgrade via V24 UserUpdateNetwork COMMAND_UPGRADECOMMANDCENTER = 9 (feat:pi.command-center)
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

    // Attempting to upgrade to level 4 (requires skill level 4) must fail with MissingSkillForCommandCenterUpgrade
    let up_skill_err = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(9), // COMMAND_UPGRADECOMMANDCENTER
                    PyRep::Integer(cc_pin_id), // Command center pin ID
                    PyRep::Integer(4), // Target level 4
                ])]),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(up_skill_err.code, "MissingSkillForCommandCenterUpgrade");

    let s1_bal_before_up1 = ctx.db.get_character(90_000_001).unwrap().balance;

    // Step 1: Upgrade Command Center from level 0 -> level 1 (580,000 ISK per planetCommon.py commandCenterInfoPerLevel)
    let up1_res = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(9), // COMMAND_UPGRADECOMMANDCENTER
                    PyRep::Integer(cc_pin_id),
                    PyRep::Integer(1), // Target level 1 (CPU: 7,057 TF, PG: 9,000 MW)
                ])]),
            ],
            &kw,
            None,
        )
        .await
        .expect("UserUpdateNetwork COMMAND_UPGRADECOMMANDCENTER level 1");
    let PyRep::KeyVal(up1_kv) = up1_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(up1_kv.get("currentLevel"), Some(&PyRep::Integer(1)));
    assert_eq!(up1_kv.get("cpuCapacity"), Some(&PyRep::Integer(7057)));
    assert_eq!(up1_kv.get("powerCapacity"), Some(&PyRep::Integer(9000)));
    let s1_bal_after_up1 = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(
        s1_bal_before_up1.0 - s1_bal_after_up1.0,
        58_000_000, // 580,000 ISK in cents
        "Upgrading Command Center 0 -> 1 must debit 580,000 ISK"
    );

    // Step 2: Upgrade Command Center from level 1 -> level 3 (skipping L2 pays 930,000 + 1,200,000 = 2,130,000 ISK; cumulative 2,710,000 ISK)
    let up_res = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(9), // COMMAND_UPGRADECOMMANDCENTER
                    PyRep::Integer(cc_pin_id), // Command center pin ID
                    PyRep::Integer(3), // Target level 3 (CPU: 17,215 TF, PG: 15,000 MW)
                ])]),
            ],
            &kw,
            None,
        )
        .await
        .expect("UserUpdateNetwork COMMAND_UPGRADECOMMANDCENTER level 3");

    let PyRep::KeyVal(up_kv) = up_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(up_kv.get("currentLevel"), Some(&PyRep::Integer(3)));
    assert_eq!(up_kv.get("cpuCapacity"), Some(&PyRep::Integer(17215)));
    assert_eq!(up_kv.get("powerCapacity"), Some(&PyRep::Integer(15000)));

    let s1_bal_after_up = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(
        s1_bal_after_up1.0 - s1_bal_after_up.0,
        213_000_000, // 2,130,000 ISK in cents (930k + 1,200k)
        "Upgrading Command Center 1 -> 3 must debit 2,130,000 ISK (sum of L2 and L3 step costs)"
    );
    assert_eq!(
        s1_bal_before_up1.0 - s1_bal_after_up.0,
        271_000_000, // 2,710,000 ISK cumulative
        "Cumulative 0 -> 3 upgrade cost must equal 2,710,000 ISK"
    );

    let cc_lvl = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "GetCommandCenterLevel",
            &[PyRep::Integer(JITA_PLANET_ID)],
            &kw,
            None,
        )
        .await
        .expect("GetCommandCenterLevel");
    assert_eq!(cc_lvl, PyRep::Integer(3));

    // 5. Place Pins & Links via V24 UserUpdateNetwork (feat:pi.place-pins, feat:pi.links)
    // Place an Extractor Control Unit (2848: 400 TF, 2600 MW) as client temp pin 2,
    // two Basic Industry Facilities (2469: 200 TF, 800 MW each) as client temp pins 3 and 4,
    // create spherical surface links (cc_pin_id -> 2) and (2 -> 3), and upgrade link (cc_pin_id -> 2) to level 2
    let bal_before_pins_links = ctx.db.get_character(90_000_001).unwrap().balance;
    let net_res = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![
                    PyRep::Tuple(vec![
                        PyRep::Integer(1), // COMMAND_CREATEPIN
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)]),
                        PyRep::Integer(2848), // Extractor Control Unit
                        PyRep::Real(0.36),
                        PyRep::Real(-1.10),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::Integer(1), // COMMAND_CREATEPIN
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(3)]),
                        PyRep::Integer(2469), // Basic Industry Facility
                        PyRep::Real(0.37),
                        PyRep::Real(-1.09),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::Integer(1), // COMMAND_CREATEPIN
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(4)]),
                        PyRep::Integer(2469), // Basic Industry Facility
                        PyRep::Real(0.38),
                        PyRep::Real(-1.08),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::Integer(3), // COMMAND_CREATELINK
                        PyRep::Integer(cc_pin_id), // source: Command Center
                        PyRep::Integer(2), // destination: Extractor (client temp ID 2)
                        PyRep::Integer(0), // initial level 0
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::Integer(3), // COMMAND_CREATELINK
                        PyRep::Integer(2), // source: Extractor (client temp ID 2)
                        PyRep::Integer(3), // destination: Basic Industry Facility (client temp ID 3)
                        PyRep::Integer(0),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::Integer(5), // COMMAND_SETLINKLEVEL
                        PyRep::Integer(cc_pin_id),
                        PyRep::Integer(2),
                        PyRep::Integer(2), // upgrade link bandwidth to level 2
                    ]),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .expect("UserUpdateNetwork place pins and links");

    let PyRep::KeyVal(net_kv) = net_res else {
        panic!("Expected KeyVal from UserUpdateNetwork");
    };
    assert_eq!(net_kv.get("pinsCount"), Some(&PyRep::Integer(4)));
    assert_eq!(net_kv.get("linksCount"), Some(&PyRep::Integer(2)));
    let bal_after_pins_links = ctx.db.get_character(90_000_001).unwrap().balance;
    let isk_spent_reported = net_kv
        .get("totalCostISK")
        .and_then(|v| v.as_f64())
        .expect("totalCostISK in UserUpdateNetwork");
    assert!(
        isk_spent_reported > 100_000.0,
        "Placing 1 ECU + 2 Basic Industry Facilities + 2 links must charge pin construction ISK ({isk_spent_reported})"
    );
    assert_eq!(
        bal_before_pins_links.0 - bal_after_pins_links.0,
        IskCents::from_f64_isk(isk_spent_reported).0,
        "Wallet balance debit must equal exact pin + link construction cost"
    );
    let Some(PyRep::List(net_pins)) = net_kv.get("pins") else {
        panic!("Expected pins list in UserUpdateNetwork response");
    };
    let proc_pin_id = net_pins
        .iter()
        .find_map(|p| match p {
            PyRep::KeyVal(kv) if kv.get("typeID").and_then(|v| v.as_i64()) == Some(2469) => {
                kv.get("pinID").and_then(|v| v.as_i64())
            }
            _ => None,
        })
        .expect("Server-allocated Basic Industry Facility pinID");
    assert!(proc_pin_id > cc_pin_id);

    // Verify exact spherical link distance and link CPU/PG cost formula (feat:pi.links)
    let Some(PyRep::List(net_links)) = net_kv.get("links") else {
        panic!("Expected links list in UserUpdateNetwork response");
    };
    assert_eq!(net_links.len(), 2);
    let mut total_link_cpu = 0.0;
    let mut total_link_pg = 0.0;
    for link_rep in net_links {
        let PyRep::KeyVal(lkv) = link_rep else {
            panic!("Expected KeyVal for link");
        };
        let lvl = lkv.get("level").and_then(|v| v.as_i64()).unwrap() as u8;
        let len_km = lkv.get("lengthKm").and_then(|v| v.as_f64()).unwrap();
        let l_cpu = lkv.get("cpuUsage").and_then(|v| v.as_f64()).unwrap();
        let l_pg = lkv.get("powergridUsage").and_then(|v| v.as_f64()).unwrap();
        let l_bw = lkv.get("bandwidth").and_then(|v| v.as_f64()).unwrap();
        let (expected_pg, expected_cpu) = eve_server::formulas::compute_pi_link_cost(len_km, lvl);
        assert!((l_cpu - expected_cpu).abs() < 1e-6, "Link CPU must match compute_pi_link_cost({len_km}, {lvl})");
        assert!((l_pg - expected_pg).abs() < 1e-6, "Link PG must match compute_pi_link_cost({len_km}, {lvl})");
        assert!((l_bw - 1_250.0 * 2.0_f64.powi(lvl as i32)).abs() < 1e-6);
        total_link_cpu += l_cpu;
        total_link_pg += l_pg;
    }
    let cpu_used = net_kv.get("cpuUsed").and_then(|v| v.as_f64()).unwrap();
    let power_used = net_kv.get("powerUsed").and_then(|v| v.as_f64()).unwrap();
    // Base pin CPU = 400 + 200 + 200 = 800 TF; Base pin PG = 2600 + 800 + 800 = 4200 MW; plus exact spherical link overhead
    assert!((cpu_used - (800.0 + total_link_cpu)).abs() < 1e-5, "Colony cpuUsed ({cpu_used}) must equal 800.0 + {total_link_cpu}");
    assert!((power_used - (4200.0 + total_link_pg)).abs() < 1e-5, "Colony powerUsed ({power_used}) must equal 4200.0 + {total_link_pg}");

    // Verify exceeding Command Center Powergrid/CPU budget (15,000 MW at L3) is rejected with InsufficientColonyCpuOrPowergrid
    let budget_err = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![
                    PyRep::Tuple(vec![
                        PyRep::Integer(1),
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(10)]),
                        PyRep::Integer(2848), // +2600 MW
                        PyRep::Real(0.40),
                        PyRep::Real(-1.05),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::Integer(1),
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(11)]),
                        PyRep::Integer(2848), // +2600 MW
                        PyRep::Real(0.41),
                        PyRep::Real(-1.05),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::Integer(1),
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(12)]),
                        PyRep::Integer(2848), // +2600 MW
                        PyRep::Real(0.42),
                        PyRep::Real(-1.05),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::Integer(1),
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(13)]),
                        PyRep::Integer(2848), // +2600 MW
                        PyRep::Real(0.43),
                        PyRep::Real(-1.05),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::Integer(1),
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(14)]),
                        PyRep::Integer(2848), // +2600 MW -> exceeds 15,000 MW!
                        PyRep::Real(0.44),
                        PyRep::Real(-1.05),
                    ]),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(budget_err.code, "InsufficientColonyCpuOrPowergrid");

    // Verify GetFullNetworkForOwner, GetExtractorsForPlanet, and GetPin reflect the persisted network
    let full_net = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "GetFullNetworkForOwner",
            &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(90_000_001)],
            &kw,
            None,
        )
        .await
        .expect("GetFullNetworkForOwner");
    let PyRep::KeyVal(fn_kv) = full_net else {
        panic!("Expected KeyVal from GetFullNetworkForOwner");
    };
    assert!(matches!(fn_kv.get("pins"), Some(PyRep::List(l)) if l.len() == 4));
    assert!(matches!(fn_kv.get("links"), Some(PyRep::List(l)) if l.len() == 2));

    let extractors = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "GetExtractorsForPlanet",
            &[PyRep::Integer(JITA_PLANET_ID)],
            &kw,
            None,
        )
        .await
        .expect("GetExtractorsForPlanet");
    assert!(matches!(extractors, PyRep::List(l) if l.len() == 1));

    // 6. Resource depletion on extraction, schematic cycle time & invalid schematic rejection,
    // storage capacity enforcement, and launchpad customs export tax (feat:pi.colony, feat:pi.pins-links)
    // R69-P1-1: Command Center is at level 3, but Customs Code Expertise (33467) is untrained (level 0),
    // so Flush must charge the full 15% tax (10% NPC + 5% POCO) = 5.0 * producedQuantity * 0.15
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().remove(&33467);
    });
    let flush_res = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "Flush",
            &[PyRep::Integer(JITA_PLANET_ID)],
            &kw,
            None,
        )
        .await
        .expect("planetMgr.Flush");
    let PyRep::KeyVal(fl_kv) = flush_res else {
        panic!("Expected KeyVal from Flush");
    };
    let extracted_type_id = fl_kv.get("producedTypeID").and_then(|v| v.as_i64()).unwrap();
    let produced_qty = fl_kv.get("producedQuantity").and_then(|v| v.as_i64()).unwrap() as f64;
    let flush_tax_isk = fl_kv.get("pocoExportTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!(
        (flush_tax_isk - (5.0 * produced_qty * 0.15)).abs() < 1e-4,
        "R69-P1-1: pocoExportTaxISK ({flush_tax_isk}) must use Customs Code Expertise (33467, level 0 -> 15% rate), not Command Center level 3"
    );

    // Verify resource quality depletion after extraction
    let res_after = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "GetPlanetResourceInfo",
            &[PyRep::Integer(JITA_PLANET_ID)],
            &kw,
            None,
        )
        .await
        .expect("GetPlanetResourceInfo after Flush");
    let PyRep::KeyVal(ra_kv) = res_after else {
        panic!("Expected KeyVal");
    };
    let Some(PyRep::List(ra_list)) = ra_kv.get("resources") else {
        panic!("Expected resources List");
    };
    let depleted_qual = ra_list
        .iter()
        .find_map(|r| match r {
            PyRep::KeyVal(kv)
                if kv.get("resourceTypeID").and_then(|v| v.as_i64()) == Some(extracted_type_id) =>
            {
                kv.get("quality").and_then(|v| v.as_f64())
            }
            _ => None,
        })
        .expect("Depleted resource quality");
    assert!(
        depleted_qual < 0.85,
        "Resource quality must deplete after extraction, got {depleted_qual}"
    );

    // Invalid schematic ID must fail with InvalidSchematic
    let bad_schem_err = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(8), // COMMAND_SETSCHEMATIC
                    PyRep::Integer(proc_pin_id), // processor pin
                    PyRep::Integer(999_999),
                ])]),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(bad_schem_err.code, "InvalidSchematic");

    // Valid P1 schematic (65: Water, 1800s cycle time)
    ctx.dispatch_rpc(
        &mut s1,
        "planetMgr",
        None,
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(8), // COMMAND_SETSCHEMATIC
                PyRep::Integer(proc_pin_id), // processor pin
                PyRep::Integer(65),
            ])]),
        ],
        &kw,
        None,
    )
    .await
    .expect("Set valid schematic 65 on processor pin");
    let pin3_info = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "GetPin",
            &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(proc_pin_id)],
            &kw,
            None,
        )
        .await
        .expect("GetPin processor");
    let PyRep::KeyVal(p3_kv) = pin3_info else {
        panic!("Expected KeyVal from GetPin");
    };
    assert_eq!(p3_kv.get("schematicID"), Some(&PyRep::Integer(65)));
    assert_eq!(p3_kv.get("cycleTimeSeconds"), Some(&PyRep::Integer(1800)));

    // Exceeding pin storage capacity via UserTransferCommodities must fail with PinStorageFull
    let cap_err = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "UserTransferCommodities",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::Integer(2288),
                PyRep::Integer(3645),
                PyRep::Integer(50_000),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(cap_err.code, "PinStorageFull");

    // Seed P1 Water (3645), train Customs Code Expertise (33467) to level 4 (-40% NPC tax -> 6% NPC + 5% POCO = 11%),
    // and verify UserLaunchCommodities to CustomsOffice deducts CustomOfficeTax at 11%
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(col) = snap.planetary_colonies.get_mut(&(90_000_001, JITA_PLANET_ID)) {
            col.extracted_commodities.insert(3645, 500);
        }
        snap.skills.entry(90_000_001).or_default().insert(
            33467,
            TrainedSkillRecord {
                type_id: 33467,
                level: 4,
                skill_points: 135_765.0,
            },
        );
    });
    let bal_before_launch = ctx.db.get_character(90_000_001).unwrap().balance;
    let launch_res = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "UserLaunchCommodities",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::Integer(3645),
                PyRep::Integer(200),
                PyRep::String("CustomsOffice".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("UserLaunchCommodities to CustomsOffice");
    let PyRep::KeyVal(ln_kv) = launch_res else {
        panic!("Expected KeyVal from UserLaunchCommodities");
    };
    let launch_tax = ln_kv.get("exportTaxISK").and_then(|v| v.as_f64()).unwrap();
    let water_base = ctx.sde.types.get(&3645).map(|t| t.base_price.max(400.0)).unwrap_or(400.0);
    assert!(
        (launch_tax - (water_base * 200.0 * 0.11)).abs() < 1e-4,
        "R69-P1-1: Launching commodities with Customs Code Expertise L4 must charge 11% export tax, got {launch_tax}"
    );
    let bal_after_launch = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(bal_before_launch.0 - bal_after_launch.0, IskCents::from_f64_isk(launch_tax).0);
    let has_poco_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal
            .iter()
            .any(|j| j.character_id == 90_000_001 && j.entry_type == "CustomOfficeTax")
    });
    assert!(has_poco_journal, "Wallet journal must record CustomOfficeTax on launchpad export");
}

