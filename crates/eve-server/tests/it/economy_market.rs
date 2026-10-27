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
    CorporationRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord, TypeId,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_STATION_ID: i64 = 60_003_760;
const TRITANIUM_TYPE_ID: TypeId = 34;
const BROKER_RELATIONS_SKILL: TypeId = 3446;
const ACCOUNTING_SKILL: TypeId = 16622;

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
    // parity: feat:market.broker-fee, feat:market.sales-tax, plan:E-1
    eve_testkit::attest(&["feat:market.broker-fee", "feat:market.sales-tax"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 101, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 102, "other", "other-secret", 90_000_002).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.standings.clear();
        }
        snap.skills.entry(90_000_001).or_default().insert(
            BROKER_RELATIONS_SKILL,
            TrainedSkillRecord { type_id: BROKER_RELATIONS_SKILL, level: 0, skill_points: 0.0 },
        );
    });

    let order_l0 = rpc(
        &ctx, &mut s1, "marketProxy", "PlaceBuyOrder",
        &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Real(1.0), PyRep::Integer(10_000), PyRep::Integer(90), PyRep::Integer(1)],
    ).await.expect("PlaceBuyOrder level 0");
    assert_eq!(order_l0.dict_get("filledImmediately").and_then(|v| v.as_bool()), Some(false));
    let fee_l0 = order_l0.dict_get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap();
    assert!((fee_l0 - 300.0).abs() < 0.01);
    let oid_l0 = order_l0.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(oid_l0)]).await.expect("CancelCharOrder");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(
            BROKER_RELATIONS_SKILL,
            TrainedSkillRecord { type_id: BROKER_RELATIONS_SKILL, level: 5, skill_points: 256_000.0 },
        );
    });

    let order_l5 = rpc(
        &ctx, &mut s1, "marketProxy", "PlaceBuyOrder",
        &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Real(1.0), PyRep::Integer(10_000), PyRep::Integer(90), PyRep::Integer(1)],
    ).await.expect("PlaceBuyOrder level 5");
    let fee_l5 = order_l5.dict_get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap();
    assert!((fee_l5 - 150.0).abs() < 0.01);
    let oid_l5 = order_l5.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(oid_l5)]).await.expect("CancelCharOrder");

    // Standing discount: -0.03% x faction standing (5.0 -> -0.15%) - 0.02% x corp standing (5.0 -> -0.10%) = 1.25% -> 125.00 ISK
    let st_def = ctx.sde.stations.get(&JITA_STATION_ID).unwrap();
    let (owner_corp, owner_fac) = (st_def.owner_corporation_id, st_def.owner_faction_id.unwrap_or(500_001));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.standings.insert(owner_fac, 5.0);
            ch.standings.insert(owner_corp, 5.0);
        }
    });
    let order_st = rpc(
        &ctx, &mut s1, "marketProxy", "PlaceBuyOrder",
        &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Real(1.0), PyRep::Integer(10_000), PyRep::Integer(90), PyRep::Integer(1)],
    ).await.expect("PlaceBuyOrder with faction+corp standing");
    assert!((order_st.dict_get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap() - 125.0).abs() < 0.01);
    let oid_st = order_st.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(oid_st)]).await.expect("CancelCharOrder");

    // Minimum 100 ISK broker fee on small order (10 x 1.0 = 10 ISK)
    let order_min = rpc(
        &ctx, &mut s1, "marketProxy", "PlaceBuyOrder",
        &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Real(1.0), PyRep::Integer(10), PyRep::Integer(90), PyRep::Integer(1)],
    ).await.expect("PlaceBuyOrder min 100 ISK fee");
    assert!((order_min.dict_get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap() - 100.0).abs() < 0.01);
    let oid_min = order_min.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(oid_min)]).await.expect("CancelCharOrder");

    // Structure broker rate (1.0% owner + 0.5% SCC surcharge = 1.5%)
    let struct_id = 1_040_000_111_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let cid = snap.characters[&90_000_001].corporation_id;
        snap.corporations.insert(cid, CorporationRecord { corporation_id: cid, name: "Mkt".into(), ticker: "MKT".into(), ceo_id: 90_000_001, ..Default::default() });
        snap.structures.insert(struct_id, eve_db::UpwellStructureRecord {
            structure_id: struct_id, type_id: 35833, owner_corp_id: cid, solar_system_id: 30_000_142,
            name: "Hub".into(), position: [0.0; 3], state: "ShieldVulnerable".into(), quantum_core_installed: true,
            fuel_blocks: 100, shield_hp: 1000.0, armor_hp: 1000.0, hull_hp: 1000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        snap.items.insert(struct_id + 1, InventoryItemRecord { item_id: struct_id + 1, type_id: 35892, owner_id: cid, location_id: struct_id, flag: InventoryFlag::ServiceSlot0, quantity: 1, singleton: true, version: 1, dynamic_attributes: HashMap::from([(2, 1.0)]), ..Default::default() });
        snap.module_online_states.insert(struct_id + 1, true);
    });
    rpc(&ctx, &mut s1, "facilityManager", "SetFacilityTaxes", &[PyRep::Integer(struct_id), PyRep::keyval([("market", PyRep::Real(0.01))])]).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
            ch.structure_id = Some(struct_id);
        }
    });
    s1.station_id = None; s1.structure_id = Some(struct_id);
    let st_ord = rpc(&ctx, &mut s1, "marketProxy", "PlaceBuyOrder", &[PyRep::Integer(struct_id), PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Real(10.0), PyRep::Integer(10_000), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false)]).await.unwrap();
    assert!((st_ord.dict_get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap() - 1_500.0).abs() < 0.01);
    let st_oid = st_ord.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(st_oid)]).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(JITA_STATION_ID);
            ch.structure_id = None;
        }
    });
    s1.station_id = Some(JITA_STATION_ID); s1.structure_id = None;

    let has_broker_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal.iter().any(|j| j.character_id == 90_000_001 && j.entry_type == "MarketBrokerFee")
    });
    assert!(has_broker_journal);

    let buy_order = rpc(
        &ctx, &mut s1, "marketProxy", "PlaceBuyOrder",
        &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Real(1.0), PyRep::Integer(200), PyRep::Integer(90), PyRep::Integer(1)],
    ).await.expect("Place buy order");
    assert_eq!(buy_order.dict_get("filledImmediately").and_then(|v| v.as_bool()), Some(false));

    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let item_id = snap.next_entity_id;
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id, type_id: TRITANIUM_TYPE_ID, owner_id: 90_000_002, location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar, quantity: 200, singleton: false, version: 1, ..Default::default()
            },
        );
        snap.skills.entry(90_000_002).or_default().insert(
            ACCOUNTING_SKILL,
            TrainedSkillRecord { type_id: ACCOUNTING_SKILL, level: 0, skill_points: 0.0 },
        );
    });

    let s2_bal_before = ctx.db.get_character(90_000_002).unwrap().balance;
    let sell_l0 = rpc(
        &ctx, &mut s2, "marketProxy", "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                ("price", PyRep::Real(1.0)),
                ("quantity", PyRep::Integer(100)),
            ])]),
            PyRep::Bool(false), PyRep::Integer(90), PyRep::Real(0.0),
        ],
    ).await.expect("PlaceMultiSellOrder sell level 0");

    assert_eq!(sell_l0.dict_get("filledImmediately").and_then(|v| v.as_bool()), Some(true));
    let tax_l0 = sell_l0.dict_get("salesTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!((tax_l0 - 14.10).abs() < 0.01);

    let broker_l0 = sell_l0.dict_get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap();
    let s2_bal_after = ctx.db.get_character(90_000_002).unwrap().balance;
    let net_gain = (s2_bal_after.0 - s2_bal_before.0) as f64 / 100.0;
    let expected_gain = 188.0 - 14.10 - broker_l0;
    assert!((net_gain - expected_gain).abs() < 0.02);

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_002).or_default().insert(
            ACCOUNTING_SKILL,
            TrainedSkillRecord { type_id: ACCOUNTING_SKILL, level: 5, skill_points: 256_000.0 },
        );
    });

    let sell_l5 = rpc(
        &ctx, &mut s2, "marketProxy", "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                ("price", PyRep::Real(1.0)),
                ("quantity", PyRep::Integer(100)),
            ])]),
            PyRep::Bool(false), PyRep::Integer(90), PyRep::Real(0.0),
        ],
    ).await.expect("PlaceMultiSellOrder sell level 5");

    assert_eq!(sell_l5.dict_get("filledImmediately").and_then(|v| v.as_bool()), Some(true));
    let tax_l5 = sell_l5.dict_get("salesTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!((tax_l5 - 6.35).abs() < 0.01);
    let has_tx_tax = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal.iter().any(|j| j.character_id == 90_000_002 && j.entry_type == "TransactionTax")
    });
    assert!(has_tx_tax, "Sales tax must be journaled as TransactionTax");
}

#[tokio::test]
async fn test_e2_market_order_matching_queries_and_cancellation() {
    // parity: feat:market.cancel-order, feat:market.price-history, plan:E-2
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
