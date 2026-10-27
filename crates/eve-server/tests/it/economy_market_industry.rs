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

    // Verify faction (-0.03%/pt) and corp (-0.02%/pt) standing discount + minimum 100 ISK broker fee
    let st_def = ctx.sde.stations.get(&JITA_STATION_ID).unwrap();
    let (owner_corp, owner_fac) = (st_def.owner_corporation_id, st_def.owner_faction_id.unwrap_or(500_001));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.standings.insert(owner_fac, 5.0);
            ch.standings.insert(owner_corp, 5.0);
        }
    });
    let order_st = ctx
        .dispatch_rpc(
            &mut s1, "marketProxy", None, "PlaceBuyOrder",
            &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Real(1.0), PyRep::Integer(10_000), PyRep::Integer(90), PyRep::Integer(1)],
            &kw, None,
        )
        .await
        .expect("PlaceBuyOrder with faction and corp standing");
    assert!((order_st.dict_get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap() - 125.0).abs() < 0.01);
    let oid_st = order_st.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(&mut s1, "marketProxy", None, "CancelCharOrder", &[PyRep::Integer(oid_st)], &kw, None).await.unwrap();

    let order_min = ctx
        .dispatch_rpc(
            &mut s1, "marketProxy", None, "PlaceBuyOrder",
            &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Real(1.0), PyRep::Integer(10), PyRep::Integer(90), PyRep::Integer(1)],
            &kw, None,
        )
        .await
        .expect("PlaceBuyOrder min 100 ISK broker fee");
    assert!((order_min.dict_get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap() - 100.0).abs() < 0.01);
    let oid_min = order_min.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(&mut s1, "marketProxy", None, "CancelCharOrder", &[PyRep::Integer(oid_min)], &kw, None).await.unwrap();

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
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            struct_id + 1,
            InventoryItemRecord {
                item_id: struct_id + 1,
                type_id: 35892, // Standup Market Hub I
                owner_id: corp_id,
                location_id: struct_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(2, 1.0)]),
                ..Default::default()
            },
        );
        snap.module_online_states.insert(struct_id + 1, true);
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
