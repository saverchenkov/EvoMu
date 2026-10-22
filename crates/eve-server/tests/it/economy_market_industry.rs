//! Market Broker Fees, Sales Tax, Effective Standing, Limit Orders, Escrow, Order Book Queries,
//! and Order Cancellation integration tests (`E-1`, `E-2`).



use crate::common;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::formulas::compute_effective_standing;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_STATION_ID: i64 = 60_003_760;
const TRITANIUM_TYPE_ID: TypeId = 34;
const BROKER_RELATIONS_SKILL: TypeId = 3446;
const ACCOUNTING_SKILL: TypeId = 16622;
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
    // parity: feat:market.broker-fee, feat:market.sales-tax, rpc:facilityManager.SetFacilityTaxes, rpc:marketProxy.CancelCharOrder, rpc:marketProxy.PlaceBuyOrder, rpc:marketProxy.PlaceMultiSellOrder
    eve_testkit::attest(&["feat:market.broker-fee", "feat:market.sales-tax"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut s1 = setup_pilot(&ctx, 101, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 102, "other", "other-secret", 90_000_002).await;

    // 1. Broker fee verification at level 0 vs level 5
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

    let has_broker_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal
            .iter()
            .any(|j| j.character_id == 90_000_001 && j.entry_type == "MarketBrokerFee")
    });
    assert!(has_broker_journal, "Wallet journal must record MarketBrokerFee");

    // 2. Sales tax verification with Accounting skill discount
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
    assert!((tax_l0 - 14.10).abs() < 0.01);

    let broker_l0 = sell_l0_kv
        .get("brokerFeeISK")
        .and_then(|v| v.as_f64())
        .expect("brokerFeeISK");
    let s2_bal_after_l0 = ctx.db.get_character(90_000_002).unwrap().balance;
    let net_l0 = (s2_bal_after_l0.0 - s2_bal_before.0) as f64 / 100.0;
    let expected_net_l0 = 188.0 - 14.10 - broker_l0;
    assert!((net_l0 - expected_net_l0).abs() < 0.02);

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

    let s2_bal_before_l5 = ctx.db.get_character(90_000_002).unwrap().balance;

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
    assert!((tax_l5 - 6.35).abs() < 0.01);

    let broker_l5 = sell_l5_kv
        .get("brokerFeeISK")
        .and_then(|v| v.as_f64())
        .expect("brokerFeeISK");
    let s2_bal_after_l5 = ctx.db.get_character(90_000_002).unwrap().balance;
    let net_l5 = (s2_bal_after_l5.0 - s2_bal_before_l5.0) as f64 / 100.0;
    let expected_net_l5 = 188.0 - 6.35 - broker_l5;
    assert!((net_l5 - expected_net_l5).abs() < 0.02);
    let has_tax_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal
            .iter()
            .any(|j| j.character_id == 90_000_002 && j.entry_type == "TransactionTax")
    });
    assert!(has_tax_journal);

    // 3. Player Upwell structure broker fee rate & 50% SCC / 50% owner split (`feat:market.upwell-orders`)
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
                solar_system_id: 30_000_142,
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
    assert!((struct_fee - 250.0).abs() < 0.01);
    let corp_wallet = ctx
        .db
        .with_snapshot(|snap| snap.corporations[&struct_corp_id].master_wallet);
    assert_eq!(corp_wallet, IskCents(12_500));
}

#[tokio::test]
async fn test_effective_standing_and_broker_discount() {
    // parity: feat:standings.effective-standing, rpc:marketProxy.CancelCharOrder, rpc:marketProxy.PlaceBuyOrder
    eve_testkit::attest(&["feat:standings.effective-standing"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut s1 = setup_pilot(&ctx, 103, "demo", "demo-secret", 90_000_001).await;

    let eff_pos = compute_effective_standing(5.0, 5, 0);
    assert!((eff_pos - 6.0).abs() < 1e-6);

    let eff_neg = compute_effective_standing(-5.0, 0, 5);
    assert!((eff_neg - (-2.0)).abs() < 1e-6);

    let eff_zero = compute_effective_standing(0.0, 0, 0);
    assert!(eff_zero.abs() < 1e-6);

    let station_def = ctx.sde.stations.get(&JITA_STATION_ID).unwrap();
    let owner_corp = station_def.owner_corporation_id;

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&90_000_001).unwrap();
        ch.standings.clear();
        ch.standings.insert(owner_corp, 5.0);
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
        snap.skills
            .entry(90_000_001)
            .or_default()
            .insert(
                CONNECTIONS_SKILL,
                TrainedSkillRecord {
                    type_id: CONNECTIONS_SKILL,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
    });

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
    assert!((fee_eff - 290.0).abs() < 0.01);

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
    // parity: feat:market.escrow, feat:market.get-orders, feat:market.place-buy, feat:market.place-sell, rpc:marketProxy.CancelCharOrder, rpc:marketProxy.GetCharEscrow, rpc:marketProxy.GetOrders, rpc:marketProxy.PlaceBuyOrder, rpc:marketProxy.PlaceMultiSellOrder
    eve_testkit::attest(&["feat:market.escrow", "feat:market.get-orders", "feat:market.place-buy", "feat:market.place-sell"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut s1 = setup_pilot(&ctx, 201, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 202, "other", "other-secret", 90_000_002).await;

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
    assert_eq!(mt_escrow, IskCents(316_406));
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
    assert_eq!(s2_hangar_after_cancel, 10_000);
}
