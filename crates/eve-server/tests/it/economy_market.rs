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
        snap.corporations.insert(cid, { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = cid;
            __corp.name = "Mkt".into();
            __corp.ticker = "MKT".into();
            __corp.ceo_id = 90_000_001;
            __corp });
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

pub(crate) mod market_instant_matching {
    // Phase 6/7 Economy & Market Lifecycle Integration Tests (`E-1`, `E-2`, `E-6`, and Global PLEX Market).
    //
    // Covers:
    // - E-1: Broker fee calculation with Broker Relations skill discount, Sales tax with Accounting skill discount,
    //   4-significant-figure tick size validation, and V24 order modification relist fees (`ModifyCharOrder`)
    // - E-2: Market order matching engine, regional/system/station queries, price history (`GetHistoryForManyTypeIDs`),
    //   order expiry & history (`GetMarketOrderHistory`), corporate orders (`GetCorporationOrders`),
    //   remote market skills (`Procurement`, `Marketing`, `Daytrading`, `Visibility`),
    //   Sleeper blue loot NPC buy orders (`StartupCheck`), and global PLEX (`44992`) market trading
    // - E-6: LP store offerings lookup and exchange



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

        // Visibility (3447) level 1 allows solar-system ranged remote buying (orderRange = 0)
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(90_000_001).or_default().insert(3447, eve_db::TrainedSkillRecord { type_id: 3447, level: 1, skill_points: 1000.0 });
        });
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
}

pub(crate) mod market_modify_relist_tick_size {
    // Phase 6/7 Economy & Market Lifecycle Integration Tests (`E-1`, `E-2`, `E-6`, and Global PLEX Market).
    //
    // Covers:
    // - E-1: Broker fee calculation with Broker Relations skill discount, Sales tax with Accounting skill discount,
    //   4-significant-figure tick size validation, and V24 order modification relist fees (`ModifyCharOrder`)
    // - E-2: Market order matching engine, regional/system/station queries, price history (`GetHistoryForManyTypeIDs`),
    //   order expiry & history (`GetMarketOrderHistory`), corporate orders (`GetCorporationOrders`),
    //   remote market skills (`Procurement`, `Marketing`, `Daytrading`, `Visibility`),
    //   Sleeper blue loot NPC buy orders (`StartupCheck`), and global PLEX (`44992`) market trading
    // - E-6: LP store offerings lookup and exchange



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
    async fn test_market_modify_relist_tick_size_corp_remote_expiry_sleeper_and_plex() {
        // parity: feat:market.modify-order, feat:market.remote-orders, feat:market.corp-orders, feat:market.plex-market, feat:market.npc-buy-orders-sleeper-loot, rpc:marketProxy.ModifyCharOrder, rpc:marketProxy.GetCorporationOrders, rpc:marketProxy.GetHistoryForManyTypeIDs, rpc:marketProxy.GetPlexBest, rpc:marketProxy.GetPlexHistory, rpc:marketProxy.GetPlexNewPriceHistory, rpc:marketProxy.GetPlexOldPriceHistory, rpc:marketProxy.GetPlexOrders, rpc:marketProxy.GetSystemAsks, rpc:marketProxy.StartupCheck
        eve_testkit::attest(&["feat:market.modify-order", "feat:market.remote-orders", "feat:market.corp-orders", "feat:market.plex-market", "feat:market.npc-buy-orders-sleeper-loot"]);
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
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
            let mut corp = { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = cid;
                __corp.name = "Trader Corp".into();
                __corp.ticker = "TRDR".into();
                __corp.ceo_id = 90_000_001;
                __corp.set_division_balance(1, IskCents::from_isk(10_000_000));
                __corp };
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
                structure_id, type_id: 35833, owner_corp_id: corp_id, solar_system_id: 30_000_142,
                name: "Jita Market Citadel".into(), position: [0.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
                quantum_core_installed: true, fuel_blocks: 100, shield_hp: 1000.0, armor_hp: 1000.0, hull_hp: 1000.0,
                reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
            snap.items.insert(structure_id + 1, InventoryItemRecord { item_id: structure_id + 1, type_id: 35892, owner_id: corp_id, location_id: structure_id, flag: InventoryFlag::ServiceSlot0, quantity: 1, singleton: true, version: 1, dynamic_attributes: HashMap::from([(2, 1.0)]), ..Default::default() });
            snap.module_online_states.insert(structure_id + 1, true);
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
        let corp_bal_before_str = ctx.db.with_snapshot(|snap| snap.corporations[&corp_id].master_wallet_balance());
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
        let corp_bal_after_str = ctx.db.with_snapshot(|snap| snap.corporations[&corp_id].master_wallet_balance());
        assert_eq!(corp_bal_after_str.0 - corp_bal_before_str.0, 15_000); // 50% (150.00 ISK) to structure owner corp wallet
    }

    #[tokio::test]
    async fn test_e6_lp_store_offerings_and_exchange() {
        // parity: feat:lp.lp-balance
        eve_testkit::attest(&["feat:lp.lp-balance"]);
        let ctx = ServerContext::new();
        crate::common::seed_test_lp_store_offers(&ctx);
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
}

pub(crate) mod market_price_history_aggregates_empty {
    // Integration tests for Market Price History behavioral verification:
    // Verifies that `marketProxy.GetOldPriceHistory`, `GetNewPriceHistory`, and
    // `GetHistoryForManyTypeIDs` query authentic trade aggregates from executed
    // market orders rather than fabricating static synthetic numbers, and that
    // un-traded types return an empty rowset.

    use crate::common;
    use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TypeId};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    const JITA_REGION_ID: u32 = 10_000_002;
    const JITA_SYSTEM_ID: u32 = 30_000_142;
    const JITA_STATION_ID: i64 = 60_003_760;
    const TRITANIUM: TypeId = 34;
    const UNTRADED_TYPE: TypeId = 99_999;

    const SELLER_CHAR_ID: i64 = 90_000_001;
    const BUYER_CHAR_ID: i64 = 90_000_002;

    async fn setup_pilot(ctx: &ServerContext, session_id: u64, username: &str, password: &str, char_id: i64) -> SessionState {
        let mut sess = SessionState::new(session_id);
        common::login_pilot(ctx, &mut sess, username, password).await;
        ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new(), None)
            .await
            .expect("SelectCharacterID");
        sess.station_id = Some(JITA_STATION_ID);
        sess.solar_system_id = Some(JITA_SYSTEM_ID);
        sess.region_id = Some(JITA_REGION_ID);
        sess
    }

    fn seed_hangar_item(snap: &mut eve_db::WorldStateSnapshot, type_id: TypeId, owner_id: i64, location_id: i64, quantity: i32) -> i64 {
        snap.next_entity_id += 1;
        let item_id = snap.next_entity_id;
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag: InventoryFlag::Hangar,
                quantity,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        item_id
    }

    /// Comprehensive test for market price history behavioral verification:
    /// Verifies authentic OHLC, volume, and order-count calculation on executed trades,
    /// and empty rowsets for un-traded types.
    #[tokio::test]
    async fn test_market_price_history_authentic_aggregates_and_empty_untraded() {
        // parity: feat:market.price-history, rpc:marketProxy.GetHistoryForManyTypeIDs, rpc:marketProxy.GetNewPriceHistory, rpc:marketProxy.GetOldPriceHistory
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        let mut seller_sess = setup_pilot(&ctx, 1201, "demo", "demo-secret", SELLER_CHAR_ID).await;
        let mut buyer_sess = setup_pilot(&ctx, 1202, "other", "other-secret", BUYER_CHAR_ID).await;

        // Seed seller with 50,000 Tritanium and buyer with 1,000,000 ISK; clear seeded NPC sell order
        let seller_item_id = ctx.db.with_mut_snapshot(|snap| {
            snap.market_orders.remove(&700_000_001);
            if let Some(ch) = snap.characters.get_mut(&SELLER_CHAR_ID) {
                ch.balance = IskCents::from_isk(100_000);
                ch.station_id = Some(JITA_STATION_ID);
                ch.solar_system_id = JITA_SYSTEM_ID;
            }
            if let Some(ch) = snap.characters.get_mut(&BUYER_CHAR_ID) {
                ch.balance = IskCents::from_isk(1_000_000);
                ch.station_id = Some(JITA_STATION_ID);
                ch.solar_system_id = JITA_SYSTEM_ID;
            }
            seed_hangar_item(snap, TRITANIUM, SELLER_CHAR_ID, JITA_STATION_ID, 50_000)
        });

        let empty_kw = HashMap::new();

        // 1. Initial State: Un-traded types must return an empty CRowset (0 rows), NOT fabricated rows
        let old_hist_before = ctx
            .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetOldPriceHistory", &[PyRep::Integer(TRITANIUM as i64)], &empty_kw, None)
            .await
            .expect("GetOldPriceHistory before trades");
        assert!(old_hist_before.is_crowset(), "GetOldPriceHistory must return valid CRowset");
        let rows_before = old_hist_before.crowset_rows().expect("crowset rows");
        assert_eq!(rows_before.len(), 0, "Un-traded item must return empty price history rowset");

        let new_hist_before = ctx
            .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetNewPriceHistory", &[PyRep::Integer(TRITANIUM as i64)], &empty_kw, None)
            .await
            .expect("GetNewPriceHistory before trades");
        assert!(new_hist_before.is_crowset(), "GetNewPriceHistory must return valid CRowset");
        let rows_new_before = new_hist_before.crowset_rows().expect("crowset rows");
        assert_eq!(rows_new_before.len(), 0, "Un-traded item must return empty price history rowset");

        let many_hist_before = ctx
            .dispatch_rpc(
                &mut buyer_sess,
                "marketProxy",
                None,
                "GetHistoryForManyTypeIDs",
                &[PyRep::List(vec![PyRep::Integer(TRITANIUM as i64), PyRep::Integer(UNTRADED_TYPE as i64)])],
                &empty_kw,
                None,
            )
            .await
            .expect("GetHistoryForManyTypeIDs before trades");
        let PyRep::Dict(entries_before) = many_hist_before else {
            panic!("Expected Dict from GetHistoryForManyTypeIDs");
        };
        assert_eq!(entries_before.len(), 2);
        for (_, crowset) in &entries_before {
            assert!(crowset.is_crowset());
            assert_eq!(crowset.crowset_rows().map(|r| r.len()), Some(0));
        }

        // 2. Execute Trade 1: Seller places sell for 400 units @ 4.00 ISK; Buyer buys 400 units @ 4.00 ISK
        let sell_order_arg = PyRep::List(vec![PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(seller_item_id)),
            (PyRep::String("typeID".into()), PyRep::Integer(TRITANIUM as i64)),
            (PyRep::String("price".into()), PyRep::Real(4.00)),
            (PyRep::String("quantity".into()), PyRep::Integer(400)),
            (PyRep::String("stationID".into()), PyRep::Integer(JITA_STATION_ID)),
            (PyRep::String("duration".into()), PyRep::Integer(90)),
        ])]);
        ctx.dispatch_rpc(&mut seller_sess, "marketProxy", None, "PlaceMultiSellOrder", &[sell_order_arg, PyRep::Bool(false)], &empty_kw, None)
            .await
            .expect("PlaceMultiSellOrder 1");

        ctx.dispatch_rpc(
            &mut buyer_sess,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM as i64),
                PyRep::Real(4.00),
                PyRep::Integer(400),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(90),
                PyRep::Bool(false),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("PlaceBuyOrder 1");

        // 3. Execute Trade 2 on the same day: Seller places sell for 500 units @ 6.00 ISK; Buyer buys 200 units @ 6.00 ISK
        let seller_item_id_2 = ctx.db.with_mut_snapshot(|snap| {
            seed_hangar_item(snap, TRITANIUM, SELLER_CHAR_ID, JITA_STATION_ID, 500)
        });
        let sell_order_arg_2 = PyRep::List(vec![PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(seller_item_id_2)),
            (PyRep::String("typeID".into()), PyRep::Integer(TRITANIUM as i64)),
            (PyRep::String("price".into()), PyRep::Real(6.00)),
            (PyRep::String("quantity".into()), PyRep::Integer(500)),
            (PyRep::String("stationID".into()), PyRep::Integer(JITA_STATION_ID)),
            (PyRep::String("duration".into()), PyRep::Integer(90)),
        ])]);
        ctx.dispatch_rpc(&mut seller_sess, "marketProxy", None, "PlaceMultiSellOrder", &[sell_order_arg_2, PyRep::Bool(false)], &empty_kw, None)
            .await
            .expect("PlaceMultiSellOrder 2");

        ctx.dispatch_rpc(
            &mut buyer_sess,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM as i64),
                PyRep::Real(6.00),
                PyRep::Integer(200),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(90),
                PyRep::Bool(false),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("PlaceBuyOrder 2");

        // 4. Verify persisted snapshot daily summary
        ctx.db.with_snapshot(|snap| {
            let hist = snap.market_history.get(&(JITA_REGION_ID, TRITANIUM)).expect("market_history entry for Tritanium");
            assert_eq!(hist.len(), 1, "Expected exactly 1 daily record for today");
            let day = &hist[0];
            assert_eq!(day.volume, 600, "Volume must equal sum of fills (400 + 200)");
            assert_eq!(day.order_count, 2, "Order count must equal number of executed trades");
            assert!((day.lowest - 4.00).abs() < 1e-4, "Lowest price must be 4.00");
            assert!((day.highest - 6.00).abs() < 1e-4, "Highest price must be 6.00");
            let expected_avg = (400.0 * 4.00 + 200.0 * 6.00) / 600.0; // 2800 / 600 = 4.66666...
            assert!((day.average - expected_avg).abs() < 1e-4, "Average price must be weighted average (4.6667)");
            assert!((day.open_isk() - 4.00).abs() < 1e-4, "Open price must be first trade price (4.00)");
            assert!((day.close_isk() - 6.00).abs() < 1e-4, "Close price must be latest trade price (6.00)");
        });

        // 5. Verify marketProxy.GetOldPriceHistory returns authentic daily OHLC aggregate
        let old_hist_after = ctx
            .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetOldPriceHistory", &[PyRep::Integer(TRITANIUM as i64)], &empty_kw, None)
            .await
            .expect("GetOldPriceHistory after trades");
        assert!(old_hist_after.is_crowset());
        let rows_after = old_hist_after.crowset_rows().expect("crowset rows");
        assert_eq!(rows_after.len(), 1, "Expected exactly 1 daily aggregate row");
        let row = &rows_after[0];
        let low_price = row.dbrow_get("lowPrice").and_then(|v| v.as_f64()).expect("lowPrice");
        let high_price = row.dbrow_get("highPrice").and_then(|v| v.as_f64()).expect("highPrice");
        let avg_price = row.dbrow_get("avgPrice").and_then(|v| v.as_f64()).expect("avgPrice");
        let vol = row.dbrow_get("volume").and_then(|v| v.as_i64()).expect("volume");
        let orders = row.dbrow_get("orders").and_then(|v| v.as_i64()).expect("orders");

        assert!((low_price - 4.00).abs() < 1e-4, "lowPrice must match executed low");
        assert!((high_price - 6.00).abs() < 1e-4, "highPrice must match executed high");
        let expected_avg = 2800.0 / 600.0;
        assert!((avg_price - expected_avg).abs() < 1e-4, "avgPrice must match volume-weighted average");
        assert_eq!(vol, 600, "volume must match executed units");
        assert_eq!(orders, 2, "orders must match executed order count");

        // 6. Verify marketProxy.GetNewPriceHistory returns identical authentic aggregate
        let new_hist_after = ctx
            .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetNewPriceHistory", &[PyRep::Integer(TRITANIUM as i64)], &empty_kw, None)
            .await
            .expect("GetNewPriceHistory after trades");
        assert!(new_hist_after.is_crowset());
        let new_rows = new_hist_after.crowset_rows().expect("crowset rows");
        assert_eq!(new_rows.len(), 1);
        let new_row = &new_rows[0];
        assert!((new_row.dbrow_get("lowPrice").unwrap().as_f64().unwrap() - 4.00).abs() < 1e-4);
        assert!((new_row.dbrow_get("highPrice").unwrap().as_f64().unwrap() - 6.00).abs() < 1e-4);
        assert!((new_row.dbrow_get("avgPrice").unwrap().as_f64().unwrap() - expected_avg).abs() < 1e-4);
        assert_eq!(new_row.dbrow_get("volume").unwrap().as_i64().unwrap(), 600);
        assert_eq!(new_row.dbrow_get("orders").unwrap().as_i64().unwrap(), 2);

        // 7. Verify marketProxy.GetHistoryForManyTypeIDs
        let many_hist_after = ctx
            .dispatch_rpc(
                &mut buyer_sess,
                "marketProxy",
                None,
                "GetHistoryForManyTypeIDs",
                &[PyRep::List(vec![PyRep::Integer(TRITANIUM as i64), PyRep::Integer(UNTRADED_TYPE as i64)])],
                &empty_kw,
                None,
            )
            .await
            .expect("GetHistoryForManyTypeIDs after trades");
        let PyRep::Dict(entries_after) = many_hist_after else {
            panic!("Expected Dict from GetHistoryForManyTypeIDs");
        };
        assert_eq!(entries_after.len(), 2);

        let trit_crowset = entries_after
            .iter()
            .find(|(k, _)| k.as_i64() == Some(TRITANIUM as i64))
            .map(|(_, v)| v)
            .expect("Tritanium entry in GetHistoryForManyTypeIDs");
        let trit_rows = trit_crowset.crowset_rows().expect("trit rows");
        assert_eq!(trit_rows.len(), 1);
        let trit_row = &trit_rows[0];
        assert!((trit_row.dbrow_get("lowPrice").unwrap().as_f64().unwrap() - 4.00).abs() < 1e-4);
        assert!((trit_row.dbrow_get("highPrice").unwrap().as_f64().unwrap() - 6.00).abs() < 1e-4);
        assert_eq!(trit_row.dbrow_get("volume").unwrap().as_i64().unwrap(), 600);
        assert_eq!(trit_row.dbrow_get("orders").unwrap().as_i64().unwrap(), 2);

        let untraded_crowset = entries_after
            .iter()
            .find(|(k, _)| k.as_i64() == Some(UNTRADED_TYPE as i64))
            .map(|(_, v)| v)
            .expect("Untraded entry in GetHistoryForManyTypeIDs");
        let untraded_rows = untraded_crowset.crowset_rows().expect("untraded rows");
        assert_eq!(untraded_rows.len(), 0, "Untraded type must return empty rowset");
    }

    const PYERITE: TypeId = 35;

    #[tokio::test]
    async fn test_market_price_history_buymultiple_and_modifyorder() {
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        let mut seller_sess = setup_pilot(&ctx, 1301, "demo", "demo-secret", SELLER_CHAR_ID).await;
        let mut buyer_sess = setup_pilot(&ctx, 1302, "other", "other-secret", BUYER_CHAR_ID).await;

        // Seed seller with Pyerite and buyer with ISK; clear any existing Pyerite market orders
        let seller_item_id = ctx.db.with_mut_snapshot(|snap| {
            snap.market_orders.retain(|_, o| o.type_id != PYERITE);
            if let Some(ch) = snap.characters.get_mut(&SELLER_CHAR_ID) {
                ch.balance = IskCents::from_isk(100_000);
                ch.station_id = Some(JITA_STATION_ID);
                ch.solar_system_id = JITA_SYSTEM_ID;
            }
            if let Some(ch) = snap.characters.get_mut(&BUYER_CHAR_ID) {
                ch.balance = IskCents::from_isk(1_000_000);
                ch.station_id = Some(JITA_STATION_ID);
                ch.solar_system_id = JITA_SYSTEM_ID;
            }
            seed_hangar_item(snap, PYERITE, SELLER_CHAR_ID, JITA_STATION_ID, 10_000)
        });

        let empty_kw = HashMap::new();

        // 1. Seller places a resting sell order: 1,000 Pyerite @ 10.00 ISK
        let sell_order_arg = PyRep::List(vec![PyRep::keyval([
            ("itemID", PyRep::Integer(seller_item_id)),
            ("typeID", PyRep::Integer(PYERITE as i64)),
            ("price", PyRep::Real(10.00)),
            ("quantity", PyRep::Integer(1_000)),
            ("stationID", PyRep::Integer(JITA_STATION_ID)),
            ("duration", PyRep::Integer(90)),
        ])]);
        ctx.dispatch_rpc(
            &mut seller_sess,
            "marketProxy",
            None,
            "PlaceMultiSellOrder",
            &[sell_order_arg, PyRep::Bool(false)],
            &empty_kw,
            None,
        )
        .await
        .expect("PlaceMultiSellOrder for Pyerite");

        // 2. Buyer executes BuyMultipleItems for 300 units @ max 12.00 ISK
        let buy_items = PyRep::List(vec![PyRep::keyval([
            ("typeID", PyRep::Integer(PYERITE as i64)),
            ("quantity", PyRep::Integer(300)),
            ("maxPrice", PyRep::Real(12.00)),
            ("stationID", PyRep::Integer(JITA_STATION_ID)),
            ("orderRange", PyRep::Integer(-1)),
        ])]);
        let buy_mult_res = ctx
            .dispatch_rpc(
                &mut buyer_sess,
                "marketProxy",
                None,
                "BuyMultipleItems",
                &[buy_items, PyRep::Bool(false)],
                &empty_kw,
                None,
            )
            .await
            .expect("BuyMultipleItems for Pyerite");
        let PyRep::KeyVal(bm_kv) = buy_mult_res else { panic!("Expected KeyVal"); };
        assert_eq!(bm_kv.get("purchasedCount").and_then(|v| v.as_i64()), Some(1));
        assert_eq!(bm_kv.get("totalQuantity").and_then(|v| v.as_i64()), Some(300));

        // 3. Verify price history and market transactions after BuyMultipleItems
        let old_hist_bm = ctx
            .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetOldPriceHistory", &[PyRep::Integer(PYERITE as i64)], &empty_kw, None)
            .await
            .expect("GetOldPriceHistory after BuyMultipleItems");
        let rows_bm = old_hist_bm.crowset_rows().expect("crowset rows");
        assert_eq!(rows_bm.len(), 1, "Expected 1 row after BuyMultipleItems");
        assert_eq!(rows_bm[0].dbrow_get("volume").unwrap().as_i64().unwrap(), 300);
        assert_eq!(rows_bm[0].dbrow_get("orders").unwrap().as_i64().unwrap(), 1);
        assert!((rows_bm[0].dbrow_get("avgPrice").unwrap().as_f64().unwrap() - 10.00).abs() < 1e-4);

        ctx.db.with_snapshot(|snap| {
            let has_bm_tx = snap.market_transactions.iter().any(|t| t.type_id == PYERITE && t.quantity == 300 && t.price.0 == 1_000);
            assert!(has_bm_tx, "Market transactions must record BuyMultipleItems fill");
        });

        // 4. Buyer places a non-matching buy order for 500 Pyerite @ 8.00 ISK (below 10.00 ISK ask)
        let buy_order_res = ctx
            .dispatch_rpc(
                &mut buyer_sess,
                "marketProxy",
                None,
                "PlaceBuyOrder",
                &[
                    PyRep::Integer(JITA_STATION_ID),
                    PyRep::Integer(PYERITE as i64),
                    PyRep::Real(8.00),
                    PyRep::Integer(500),
                    PyRep::Integer(-1),
                    PyRep::Integer(1),
                    PyRep::Integer(90),
                    PyRep::Bool(false),
                ],
                &empty_kw,
                None,
            )
            .await
            .expect("PlaceBuyOrder for Pyerite @ 8.00 ISK");
        let PyRep::KeyVal(bo_kv) = buy_order_res else { panic!("Expected KeyVal"); };
        let buy_oid = bo_kv.get("orderID").and_then(|v| v.as_i64()).expect("orderID");

        // Backdate order issue_unix by 301s to bypass modification cooldown
        let now_unix = ctx.now_unix();
        let meta_nid = -(4_000_000_000_000_i64.saturating_add(buy_oid.abs()));
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(note) = snap.owner_notes.get_mut(&meta_nid) {
                note.note = format!("{{\"range\":-1,\"duration\":90,\"issue_unix\":{}}}", now_unix.saturating_sub(400));
            }
        });

        // 5. Buyer modifies the order to 10.00 ISK, crossing the resting ask of 10.00 ISK
        let mod_res = ctx
            .dispatch_rpc(
                &mut buyer_sess,
                "marketProxy",
                None,
                "ModifyCharOrder",
                &[PyRep::Integer(buy_oid), PyRep::Real(10.00)],
                &empty_kw,
                None,
            )
            .await
            .expect("ModifyCharOrder to 10.00 ISK");
        let PyRep::KeyVal(mo_kv) = mod_res else { panic!("Expected KeyVal"); };
        assert_eq!(mo_kv.get("orderID").and_then(|v| v.as_i64()), Some(buy_oid));

        // 6. Verify combined price history in GetOldPriceHistory and GetNewPriceHistory:
        // Volume was 300, now + 500 = 800; Orders was 1, now + 1 = 2; Average = 10.00 ISK
        let old_hist_mod = ctx
            .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetOldPriceHistory", &[PyRep::Integer(PYERITE as i64)], &empty_kw, None)
            .await
            .expect("GetOldPriceHistory after ModifyCharOrder");
        let rows_mod = old_hist_mod.crowset_rows().expect("crowset rows");
        assert_eq!(rows_mod.len(), 1, "Expected 1 aggregate row");
        assert_eq!(rows_mod[0].dbrow_get("volume").unwrap().as_i64().unwrap(), 800, "Volume must reflect BuyMultipleItems + ModifyCharOrder fills");
        assert_eq!(rows_mod[0].dbrow_get("orders").unwrap().as_i64().unwrap(), 2, "Orders count must be 2");
        assert!((rows_mod[0].dbrow_get("avgPrice").unwrap().as_f64().unwrap() - 10.00).abs() < 1e-4);

        let new_hist_mod = ctx
            .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetNewPriceHistory", &[PyRep::Integer(PYERITE as i64)], &empty_kw, None)
            .await
            .expect("GetNewPriceHistory after ModifyCharOrder");
        let new_rows_mod = new_hist_mod.crowset_rows().expect("crowset rows");
        assert_eq!(new_rows_mod.len(), 1);
        assert_eq!(new_rows_mod[0].dbrow_get("volume").unwrap().as_i64().unwrap(), 800);
        assert_eq!(new_rows_mod[0].dbrow_get("orders").unwrap().as_i64().unwrap(), 2);

        ctx.db.with_snapshot(|snap| {
            let has_mod_tx = snap.market_transactions.iter().any(|t| t.type_id == PYERITE && t.quantity == 500 && t.price.0 == 1_000);
            assert!(has_mod_tx, "Market transactions must record ModifyCharOrder crossed fill");
        });
    }

    const MEXALLON: TypeId = 36;

    #[tokio::test]
    async fn test_market_modify_sell_order_crossing() {
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        let mut seller_sess = setup_pilot(&ctx, 1401, "demo", "demo-secret", SELLER_CHAR_ID).await;
        let mut buyer_sess = setup_pilot(&ctx, 1402, "other", "other-secret", BUYER_CHAR_ID).await;

        // Seed seller with Mexallon and buyer with ISK; clear existing Mexallon market orders
        let seller_item_id = ctx.db.with_mut_snapshot(|snap| {
            snap.market_orders.retain(|_, o| o.type_id != MEXALLON);
            if let Some(ch) = snap.characters.get_mut(&SELLER_CHAR_ID) {
                ch.balance = IskCents::from_isk(100_000);
                ch.station_id = Some(JITA_STATION_ID);
                ch.solar_system_id = JITA_SYSTEM_ID;
            }
            if let Some(ch) = snap.characters.get_mut(&BUYER_CHAR_ID) {
                ch.balance = IskCents::from_isk(1_000_000);
                ch.station_id = Some(JITA_STATION_ID);
                ch.solar_system_id = JITA_SYSTEM_ID;
            }
            seed_hangar_item(snap, MEXALLON, SELLER_CHAR_ID, JITA_STATION_ID, 10_000)
        });

        let empty_kw = HashMap::new();

        // 1. Buyer places resting buy order for 400 Mexallon @ 50.00 ISK
        let buy_res = ctx
            .dispatch_rpc(
                &mut buyer_sess, "marketProxy", None, "PlaceBuyOrder",
                &[
                    PyRep::Integer(JITA_STATION_ID), PyRep::Integer(MEXALLON as i64),
                    PyRep::Real(50.00), PyRep::Integer(400), PyRep::Integer(-1),
                    PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
                ],
                &empty_kw, None,
            )
            .await
            .expect("PlaceBuyOrder 400 Mexallon @ 50.00");
        let PyRep::KeyVal(bo_kv) = buy_res else { panic!("Expected KeyVal"); };
        let _buy_oid = bo_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();

        // 2. Seller places sell order for 400 Mexallon @ 60.00 ISK (does not match 50.00 ISK bid)
        let sell_order_arg = PyRep::List(vec![PyRep::keyval([
            ("itemID", PyRep::Integer(seller_item_id)),
            ("typeID", PyRep::Integer(MEXALLON as i64)),
            ("price", PyRep::Real(60.00)),
            ("quantity", PyRep::Integer(400)),
            ("stationID", PyRep::Integer(JITA_STATION_ID)),
            ("duration", PyRep::Integer(90)),
        ])]);
        let sell_res = ctx
            .dispatch_rpc(
                &mut seller_sess, "marketProxy", None, "PlaceMultiSellOrder",
                &[sell_order_arg, PyRep::Bool(false)], &empty_kw, None,
            )
            .await
            .expect("PlaceMultiSellOrder @ 60.00");
        let PyRep::KeyVal(so_kv) = sell_res else { panic!("Expected KeyVal"); };
        let PyRep::List(oids) = so_kv.get("orderIDs").unwrap() else { panic!("Expected orderIDs list"); };
        let sell_oid = oids[0].as_i64().unwrap();

        // 3. Backdate sell order by 301s to bypass modification cooldown
        let now_unix = ctx.now_unix();
        let meta_nid = -(4_000_000_000_000_i64.saturating_add(sell_oid.abs()));
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(note) = snap.owner_notes.get_mut(&meta_nid) {
                note.note = format!("{{\"range\":-1,\"duration\":90,\"issue_unix\":{}}}", now_unix.saturating_sub(400));
            }
        });

        // 4. Seller modifies sell order price downward to 45.00 ISK (<= 50.00 ISK resting bid)
        // Symmetric sell order crossing: matches resting buy order @ 50.00 ISK
        let mod_sell_res = ctx
            .dispatch_rpc(
                &mut seller_sess, "marketProxy", None, "ModifyCharOrder",
                &[PyRep::Integer(sell_oid), PyRep::Real(45.00)], &empty_kw, None,
            )
            .await
            .expect("ModifyCharOrder to 45.00 ISK");
        let PyRep::KeyVal(mso_kv) = mod_sell_res else { panic!("Expected KeyVal"); };
        assert_eq!(mso_kv.get("orderID").and_then(|v| v.as_i64()), Some(sell_oid));
        assert_eq!(mso_kv.get("escrowISK").and_then(|v| v.as_f64()), Some(0.0));

        // Verify price history recorded fill @ 50.00 ISK for 400 units
        let hist = ctx
            .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetOldPriceHistory", &[PyRep::Integer(MEXALLON as i64)], &empty_kw, None)
            .await
            .expect("GetOldPriceHistory after sell order crossing");
        let rows = hist.crowset_rows().expect("crowset rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].dbrow_get("volume").unwrap().as_i64().unwrap(), 400);
        assert_eq!(rows[0].dbrow_get("orders").unwrap().as_i64().unwrap(), 1);
        assert!((rows[0].dbrow_get("avgPrice").unwrap().as_f64().unwrap() - 50.00).abs() < 1e-4);

        ctx.db.with_snapshot(|snap| {
            let has_buyer_items = snap.items.values().any(|i| i.owner_id == BUYER_CHAR_ID && i.type_id == MEXALLON && i.quantity == 400);
            assert!(has_buyer_items, "Buyer must receive 400 Mexallon items in hangar");
        });
    }

    const ISOGEN: TypeId = 37;

    #[tokio::test]
    async fn test_market_modify_order_margin_shortfall() {
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        let mut seller_sess = setup_pilot(&ctx, 1501, "demo", "demo-secret", SELLER_CHAR_ID).await;
        let mut buyer_sess = setup_pilot(&ctx, 1502, "other", "other-secret", BUYER_CHAR_ID).await;

        // Seed seller with Isogen, buyer with Margin Trading skill (16597 = lvl 5, escrow_fraction = 0.75^5)
        let seller_item_id = ctx.db.with_mut_snapshot(|snap| {
            snap.market_orders.retain(|_, o| o.type_id != ISOGEN);
            snap.skills.entry(BUYER_CHAR_ID).or_default().insert(16597, eve_db::TrainedSkillRecord {
                type_id: 16597, level: 5, skill_points: 1_280_000.0,
            });
            if let Some(ch) = snap.characters.get_mut(&SELLER_CHAR_ID) {
                ch.balance = IskCents::from_isk(100_000);
                ch.station_id = Some(JITA_STATION_ID);
                ch.solar_system_id = JITA_SYSTEM_ID;
            }
            if let Some(ch) = snap.characters.get_mut(&BUYER_CHAR_ID) {
                ch.balance = IskCents::from_isk(1_000);
                ch.station_id = Some(JITA_STATION_ID);
                ch.solar_system_id = JITA_SYSTEM_ID;
            }
            seed_hangar_item(snap, ISOGEN, SELLER_CHAR_ID, JITA_STATION_ID, 10_000)
        });

        let empty_kw = HashMap::new();

        // 1. Seller places ask for 100 Isogen @ 100.00 ISK
        let sell_order_arg = PyRep::List(vec![PyRep::keyval([
            ("itemID", PyRep::Integer(seller_item_id)),
            ("typeID", PyRep::Integer(ISOGEN as i64)),
            ("price", PyRep::Real(100.00)),
            ("quantity", PyRep::Integer(100)),
            ("stationID", PyRep::Integer(JITA_STATION_ID)),
            ("duration", PyRep::Integer(90)),
        ])]);
        ctx.dispatch_rpc(
            &mut seller_sess, "marketProxy", None, "PlaceMultiSellOrder",
            &[sell_order_arg, PyRep::Bool(false)], &empty_kw, None,
        ).await.expect("PlaceMultiSellOrder Isogen @ 100.00");

        // 2. Buyer places buy order for 100 Isogen @ 10.00 ISK
        let buy_res = ctx
            .dispatch_rpc(
                &mut buyer_sess, "marketProxy", None, "PlaceBuyOrder",
                &[
                    PyRep::Integer(JITA_STATION_ID), PyRep::Integer(ISOGEN as i64),
                    PyRep::Real(10.00), PyRep::Integer(100), PyRep::Integer(-1),
                    PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
                ],
                &empty_kw, None,
            )
            .await
            .expect("PlaceBuyOrder Isogen @ 10.00");
        let PyRep::KeyVal(bo_kv) = buy_res else { panic!("Expected KeyVal"); };
        let buy_oid = bo_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();

        // Drain buyer balance so buyer cannot cover shortfall
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&BUYER_CHAR_ID) {
                ch.balance = IskCents(0);
            }
            let now_unix = ctx.now_unix();
            let meta_nid = -(4_000_000_000_000_i64.saturating_add(buy_oid.abs()));
            if let Some(note) = snap.owner_notes.get_mut(&meta_nid) {
                note.note = format!("{{\"range\":-1,\"duration\":90,\"issue_unix\":{}}}", now_unix.saturating_sub(400));
            }
        });

        // 3. Buyer attempts to modify order to 100.00 ISK (crossing the 100.00 ISK ask).
        // Buyer has 0 ISK in wallet, so modification must fail with InsufficientFunds!
        let err = ctx
            .dispatch_rpc(
                &mut buyer_sess, "marketProxy", None, "ModifyCharOrder",
                &[PyRep::Integer(buy_oid), PyRep::Real(100.00)], &empty_kw, None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "InsufficientFunds");
    }
}

#[allow(dead_code, unused_imports)]
pub(crate) mod market_broker_orders {
    use super::*;

//  Integration tests closing canonical plan rows E-2 and E-4
//  and their blocking lifecycle and structure features.


use crate::common;
use eve_db::{
    corp_roles, CorporationRecord, IndustryJobRecord, InventoryFlag, InventoryItemRecord, IskCents,
    MarketOrderRecord, OwnerNoteRecord, TrainedSkillRecord, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::industry_pi::*;
use eve_server::services::station_structure::structure_upwell_ops::structure_allows_market_orders;
use std::collections::HashMap;

const JITA_SYSTEM_ID: u32 = 30_000_142;
const JITA_REGION_ID: i64 = 10_000_002;
const JITA_STATION_ID: i64 = 60_003_760;
const AMARR_SYSTEM_ID: u32 = 30_002_187;
const AMARR_REGION_ID: i64 = 10_000_043;
const AMARR_STATION_ID: i64 = 60_008_494;
const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const CORP_ID: i64 = 2001;

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
}

fn make_job(job_id: i64, facility_id: i64, bp_id: i64, duration_seconds: u64, start_unix: u64) -> IndustryJobRecord {
    IndustryJobRecord {
        job_id, installer_id: P1, facility_id, blueprint_item_id: bp_id, blueprint_type_id: 681,
        activity_id: 1, runs: 1, product_type_id: 587, status: "Active".into(), duration_seconds, start_unix,
        ..Default::default()
    }
}

fn insert_expired_order(snap: &mut eve_db::WorldStateSnapshot, oid: i64, owner: i64, tid: u32, is_bid: bool, price: IskCents, qty: i32, escrow: IskCents, now_u: u64) {
    snap.market_orders.insert(
        oid,
        MarketOrderRecord {
            order_id: oid, owner_id: owner, station_id: AMARR_STATION_ID, region_id: AMARR_REGION_ID as u32,
            type_id: tid, is_bid, price, vol_entered: qty, vol_remaining: qty, min_volume: 1, is_npc: false, escrow,
        },
    );
    let nid = -(4_000_000_000_000_i64.saturating_add(oid.abs()));
    snap.owner_notes.insert(
        nid,
        OwnerNoteRecord {
            note_id: nid, character_id: -4, item_id: oid, label: format!("MarketOrderMeta:{oid}"),
            note: format!("{{\"range\":32767,\"duration\":1,\"issue_unix\":{}}}", now_u.saturating_sub(86_500)), timestamp_unix: now_u,
        },
    );
}

#[tokio::test]
async fn test_e2_structure_market_service_and_order_expiry() {
    // parity: plan:E-2, feat:structures.service-market, feat:lifecycle.market_order-expiry-trigger
    eve_testkit::attest(&[
        "plan:E-2",
        "feat:structures.service-market",
        "feat:lifecycle.market_order-expiry-trigger",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let struct_id = 1_000_000_101i64;
    let struct_trit_item_id = 910_090i64;

    // 1. Structure market service: Fortizar without Standup Market Hub refuses PlaceMultiSellOrder
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.region_id != AMARR_REGION_ID as u32);
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.corporation_id = CORP_ID;
            ch.balance = IskCents(50_000_000_00);
            ch.station_id = None;
            ch.structure_id = Some(struct_id);
            ch.solar_system_id = JITA_SYSTEM_ID;
            ch.region_id = JITA_REGION_ID as u32;
        }
        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = CORP_ID;
            __corp.name = "Market Test Corp".into();
            __corp.ticker = "MKTC".into();
            __corp.ceo_id = P1;
            __corp.alliance_id = None;
            __corp.tax_rate = 0.05;
            __corp.description = String::new();
            __corp.url = String::new();
            __corp.hq_station_id = Some(AMARR_STATION_ID);
            __corp.members = [P1].into_iter().collect();
            __corp.member_roles = HashMap::from([(P1, corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_TRADER)]);
            __corp.applications = HashMap::new();
            __corp.wallet_divisions = Vec::new();
            let __mw = IskCents(50_000_000_00);
            if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); }
            __corp };
        corp.ensure_wallet_divisions();
        corp.wallet_divisions[0].balance = IskCents(50_000_000_00);
        snap.corporations.insert(CORP_ID, corp);

        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id, type_id: 35833, owner_corp_id: CORP_ID, solar_system_id: JITA_SYSTEM_ID,
                name: "Test Fortizar".into(), position: [0.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
                quantum_core_installed: true, fuel_blocks: 2_000, shield_hp: 10000.0, armor_hp: 10000.0, hull_hp: 10000.0,
                reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
        snap.structures.insert(
            struct_id + 1,
            UpwellStructureRecord {
                structure_id: struct_id + 1, type_id: 35832, owner_corp_id: CORP_ID, solar_system_id: JITA_SYSTEM_ID,
                name: "Test Astrahus".into(), position: [10_000.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
                quantum_core_installed: true, fuel_blocks: 2_000, shield_hp: 10000.0, armor_hp: 10000.0, hull_hp: 10000.0,
                reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
        let mut ast_hub = make_item(struct_id + 12, 35892, CORP_ID, struct_id + 1, InventoryFlag::ServiceSlot0, 1, true);
        ast_hub.dynamic_attributes.insert(2, 1.0);
        snap.items.insert(struct_id + 12, ast_hub);
        snap.module_online_states.insert(struct_id + 12, true);
        snap.items.insert(
            struct_trit_item_id,
            make_item(struct_trit_item_id, 34, P1, struct_id, InventoryFlag::Hangar, 1_000, false),
        );
        snap.items.insert(
            struct_trit_item_id + 1,
            make_item(struct_trit_item_id + 1, 34, P2, struct_id, InventoryFlag::Hangar, 1_000, false),
        );
    });

    assert!(
        !ctx.db.with_snapshot(|snap| structure_allows_market_orders(&ctx, snap, struct_id)),
        "Structure with zero fitted service modules must not allow market orders"
    );
    assert!(
        !ctx.db.with_snapshot(|snap| structure_allows_market_orders(&ctx, snap, struct_id + 1)),
        "Medium Astrahus must not allow market orders"
    );

    let mut sess = SessionState::new(8301);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)])
        .await
        .expect("SelectCharacterID");
    sess.corporation_id = Some(CORP_ID);
    sess.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_TRADER;

    let sell_entry_at_struct = PyRep::List(vec![PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(struct_trit_item_id)),
        (PyRep::String("typeID".into()), PyRep::Integer(34)),
        (PyRep::String("price".into()), PyRep::Real(500.0)),
        (PyRep::String("quantity".into()), PyRep::Integer(100)),
        (PyRep::String("stationID".into()), PyRep::Integer(struct_id)),
        (PyRep::String("duration".into()), PyRep::Integer(90)),
    ])]);
    let no_hub_sell_err = rpc(&ctx, &mut sess, "marketProxy", "PlaceMultiSellOrder", &[sell_entry_at_struct.clone(), PyRep::Bool(false)])
        .await.expect_err("PlaceMultiSellOrder at structure with zero service modules must fail");
    assert_eq!(no_hub_sell_err.code, "MarketHubOffline");
    let no_hub_buy_err = rpc(&ctx, &mut sess, "marketProxy", "PlaceBuyOrder", &[
        PyRep::Integer(struct_id), PyRep::Integer(34), PyRep::Real(10.0), PyRep::Integer(100),
        PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
    ]).await.expect_err("PlaceBuyOrder at structure with zero service modules must fail");
    assert_eq!(no_hub_buy_err.code, "MarketHubOffline");

    // Medium Astrahus also rejects PlaceBuyOrder
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&P1) { ch.structure_id = Some(struct_id + 1); } });
    sess.structure_id = Some(struct_id + 1);
    let med_buy_err = rpc(&ctx, &mut sess, "marketProxy", "PlaceBuyOrder", &[
        PyRep::Integer(struct_id + 1), PyRep::Integer(34), PyRep::Real(10.0), PyRep::Integer(100),
        PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
    ]).await.expect_err("PlaceBuyOrder at Medium Astrahus must fail");
    assert_eq!(med_buy_err.code, "MarketHubOffline");
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&P1) { ch.structure_id = Some(struct_id); } });
    sess.structure_id = Some(struct_id);

    // Install and online Standup Market Hub I (35892), configure structure profile with 2.0% market tax and block P2
    ctx.db.with_mut_snapshot(|snap| {
        let mut hub = make_item(struct_id + 11, 35892, CORP_ID, struct_id, InventoryFlag::ServiceSlot1, 1, true);
        hub.dynamic_attributes.insert(2, 1.0);
        snap.items.insert(struct_id + 11, hub);
        snap.module_online_states.insert(struct_id + 11, true);
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        let prof = ext.access_profiles.entry(struct_id).or_default();
        prof.service_settings.insert(4, 200); // SETTING_MARKET_TAX (4): 200 bps = 2.0% owner market rate (+ 0.5% SCC = 2.5% total)
        prof.blocked_characters.insert(P2);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
        if let Some(ch2) = snap.characters.get_mut(&P2) {
            ch2.station_id = None; ch2.structure_id = Some(struct_id);
            ch2.solar_system_id = JITA_SYSTEM_ID; ch2.region_id = JITA_REGION_ID as u32;
            ch2.balance = IskCents(10_000_000_00);
        }
    });
    assert!(ctx.db.with_snapshot(|snap| structure_allows_market_orders(&ctx, snap, struct_id)));

    let mut sess_p2 = SessionState::new(8305);
    common::login_pilot(&ctx, &mut sess_p2, "other", "other-secret").await;
    rpc(&ctx, &mut sess_p2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P2)]).await.expect("SelectCharacterID P2");
    let acl_buy_err = rpc(&ctx, &mut sess_p2, "marketProxy", "PlaceBuyOrder", &[
        PyRep::Integer(struct_id), PyRep::Integer(34), PyRep::Real(10.0), PyRep::Integer(100),
        PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
    ]).await.expect_err("ACL-blocked character must be rejected by PlaceBuyOrder");
    assert_eq!(acl_buy_err.code, "StructureServiceAccessDenied");
    let acl_sell_err = rpc(&ctx, &mut sess_p2, "marketProxy", "PlaceMultiSellOrder", &[
        PyRep::List(vec![PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(struct_trit_item_id + 1)),
            (PyRep::String("typeID".into()), PyRep::Integer(34)),
            (PyRep::String("price".into()), PyRep::Real(500.0)),
            (PyRep::String("quantity".into()), PyRep::Integer(100)),
            (PyRep::String("stationID".into()), PyRep::Integer(struct_id)),
            (PyRep::String("duration".into()), PyRep::Integer(90)),
        ])]),
        PyRep::Bool(false),
    ]).await.expect_err("ACL-blocked character must be rejected by PlaceMultiSellOrder");
    assert_eq!(acl_sell_err.code, "StructureServiceAccessDenied");

    let struct_sell_rep = rpc(&ctx, &mut sess, "marketProxy", "PlaceMultiSellOrder", &[sell_entry_at_struct, PyRep::Bool(false)])
        .await.expect("PlaceMultiSellOrder at structure with online Standup Market Hub");
    let struct_oid = struct_sell_rep.dict_get("orderID").and_then(|v| v.as_i64()).expect("struct_oid");
    let struct_buy_rep = rpc(&ctx, &mut sess, "marketProxy", "PlaceBuyOrder", &[
        PyRep::Integer(struct_id), PyRep::Integer(34), PyRep::Real(10.0), PyRep::Integer(1_000),
        PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
    ]).await.expect("PlaceBuyOrder at structure with profile market broker fee");
    let buy_fee = struct_buy_rep.dict_get("brokerFeeISK").and_then(|v| v.as_f64()).expect("brokerFeeISK");
    assert!((buy_fee - 250.0).abs() < 0.02, "Profile 2.0% + 0.5% SCC = 2.5% on 10,000 ISK = 250 ISK (got {buy_fee})");
    let jita_orders = rpc(&ctx, &mut sess, "marketProxy", "GetOrders", &[PyRep::Integer(34)]).await.expect("GetOrders in Jita");
    let jita_orders_str = format!("{jita_orders:?}");
    assert!(jita_orders_str.contains(&struct_oid.to_string()) && jita_orders_str.contains(&struct_id.to_string()));

    // 2. Order expiry lifecycle at non-Jita station (Amarr 60_008_494):
    let now_u = ctx.now_unix();
    let buy_order_id = 910_001i64;
    let sell_order_id = 910_002i64;
    let corp_order_id = 910_003i64;
    let p2_exp_bid_id = 910_004i64;
    let p2_exp_ask_id = 910_005i64;
    let p1_mex_item_id = 910_095i64;

    let seed_p2_expired = |ctx: &ServerContext| {
        ctx.db.with_mut_snapshot(|snap| {
            insert_expired_order(snap, p2_exp_bid_id, P2, 36, true, IskCents(500_00), 40, IskCents(20_000_00), now_u);
            insert_expired_order(snap, p2_exp_ask_id, P2, 36, false, IskCents(50_00), 40, IskCents(0), now_u);
        });
    };

    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.region_id != AMARR_REGION_ID as u32);
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.balance = IskCents(10_000_000_00);
            ch.station_id = Some(AMARR_STATION_ID);
            ch.structure_id = None;
            ch.solar_system_id = AMARR_SYSTEM_ID;
            ch.region_id = AMARR_REGION_ID as u32;
        }
        if let Some(ch2) = snap.characters.get_mut(&P2) {
            ch2.balance = IskCents(10_000_000_00);
        }
        snap.items.insert(
            p1_mex_item_id,
            make_item(p1_mex_item_id, 36, P1, AMARR_STATION_ID, InventoryFlag::Hangar, 100, false),
        );
        insert_expired_order(snap, buy_order_id, P1, 35, true, IskCents(100_00), 100, IskCents(10_000_00), now_u);
        insert_expired_order(snap, sell_order_id, P1, 35, false, IskCents(200_00), 50, IskCents(0), now_u);
        insert_expired_order(snap, corp_order_id, CORP_ID, 35, false, IskCents(200_00), 25, IskCents(0), now_u);
    });
    seed_p2_expired(&ctx);

    // Login P1 at Amarr -> sweeps P1's (and P1's corp's) expired orders while leaving P2's expired orders unswept
    let _ = ctx.drain_notifications_for_char(P1);
    let mut sess_amarr = SessionState::new(8302);
    common::login_pilot(&ctx, &mut sess_amarr, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess_amarr, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)])
        .await
        .expect("SelectCharacterID at Amarr");
    sess_amarr.corporation_id = Some(CORP_ID);
    sess_amarr.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_TRADER;

    assert_eq!(
        ctx.db.with_snapshot(|s| s.market_orders.get(&p2_exp_ask_id).map(|o| o.vol_remaining)),
        Some(40),
        "P1 login must only sweep P1/corp expired orders, not global P2 orders"
    );

    // 2a. Verify all 5 read RPCs never return expired orders even when unswept expired orders exist on entry
    for tid in [35i64, 36i64] {
        seed_p2_expired(&ctx);
        let orders_rep = rpc(&ctx, &mut sess_amarr, "marketProxy", "GetOrders", &[PyRep::Integer(tid)]).await.expect("GetOrders");
        let s = format!("{orders_rep:?}");
        for oid in [buy_order_id, sell_order_id, corp_order_id, p2_exp_bid_id, p2_exp_ask_id] {
            assert!(!s.contains(&oid.to_string()), "GetOrders must not return expired order {oid}");
        }
    }

    seed_p2_expired(&ctx);
    let region_best = rpc(&ctx, &mut sess_amarr, "marketProxy", "GetRegionBest", &[]).await.expect("GetRegionBest");
    let rb_str = format!("{region_best:?}");
    assert!(!rb_str.contains("Integer(35)") && !rb_str.contains("Integer(36)"), "GetRegionBest must exclude expired asks");

    seed_p2_expired(&ctx);
    let station_asks = rpc(&ctx, &mut sess_amarr, "marketProxy", "GetStationAsks", &[]).await.expect("GetStationAsks");
    let sa_str = format!("{station_asks:?}");
    assert!(!sa_str.contains("Integer(35)") && !sa_str.contains("Integer(36)"), "GetStationAsks must exclude expired asks");

    let char_orders = rpc(&ctx, &mut sess_amarr, "marketProxy", "GetCharOrders", &[]).await.expect("GetCharOrders");
    let co_str = format!("{char_orders:?}");
    assert!(!co_str.contains(&buy_order_id.to_string()) && !co_str.contains(&sell_order_id.to_string()));

    let corp_orders = rpc(&ctx, &mut sess_amarr, "marketProxy", "GetCorporationOrders", &[]).await.expect("GetCorporationOrders");
    assert!(!format!("{corp_orders:?}").contains(&corp_order_id.to_string()));

    // 2b. Verify all 3 match RPCs never match against unswept expired orders (910_004 bid @ 500 ISK, 910_005 ask @ 50 ISK)
    let p1_bal_before_match = ctx.db.get_character(P1).unwrap().balance.0;

    // (i) PlaceBuyOrder (immediate duration=0 @ 100 ISK against expired ask @ 50 ISK)
    seed_p2_expired(&ctx);
    let buy_match_res = rpc(
        &ctx,
        &mut sess_amarr,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(AMARR_STATION_ID),
            PyRep::Integer(36),
            PyRep::Real(100.0),
            PyRep::Integer(10),
            PyRep::Integer(32767),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Bool(false),
        ],
    )
    .await;
    assert!(
        buy_match_res.is_err()
            || buy_match_res.as_ref().ok().and_then(|r| r.dict_get("filledImmediately")).and_then(|v| v.as_bool()) == Some(false)
    );

    // (ii) PlaceMultiSellOrder (immediate duration=0 @ 100 ISK against expired bid @ 500 ISK)
    seed_p2_expired(&ctx);
    let sell_match_err = rpc(
        &ctx,
        &mut sess_amarr,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::Dict(vec![
                (PyRep::String("itemID".into()), PyRep::Integer(p1_mex_item_id)),
                (PyRep::String("typeID".into()), PyRep::Integer(36)),
                (PyRep::String("price".into()), PyRep::Real(100.0)),
                (PyRep::String("quantity".into()), PyRep::Integer(10)),
                (PyRep::String("stationID".into()), PyRep::Integer(AMARR_STATION_ID)),
                (PyRep::String("duration".into()), PyRep::Integer(0)),
            ])]),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect_err("Immediate PlaceMultiSellOrder must not fill against expired bid order");
    assert_eq!(sell_match_err.code, "ImmediateOrderUnfilled");

    // (iii) BuyMultipleItems (@ 100 ISK against expired ask @ 50 ISK)
    seed_p2_expired(&ctx);
    let bmi_res = rpc(
        &ctx,
        &mut sess_amarr,
        "marketProxy",
        "BuyMultipleItems",
        &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(36), PyRep::Integer(10), PyRep::Real(100.0)])])],
    )
    .await;
    if let Ok(ref rep) = bmi_res {
        assert_eq!(rep.dict_get("purchasedCount").and_then(|v| v.as_i64()).unwrap_or(0), 0);
    }
    assert_eq!(ctx.db.get_character(P1).unwrap().balance.0, p1_bal_before_match, "No P1 ISK moved on expired order match attempts");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&p1_mex_item_id).map(|i| i.quantity)), Some(100));

    // 2c. Verify P1's login sweep refunded buy escrow, returned sell items to Amarr hangar, and emitted notifications
    assert_eq!(p1_bal_before_match, 10_000_000_00 + 10_000_00, "Buy escrow refunded exactly once");
    let returned_pyerite = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| i.location_id == AMARR_STATION_ID && i.type_id == 35 && i.flag == InventoryFlag::Hangar && i.owner_id == P1)
            .map(|i| i.quantity)
    });
    assert_eq!(returned_pyerite, Some(50), "Sell items returned to non-Jita order station hangar (Amarr)");

    let notifs = ctx.drain_notifications_for_char(P1);
    assert!(notifs.iter().any(|n| n.event == "OnOwnOrdersChanged"));
    let item_recv = notifs.iter().find(|n| n.event == "OnMarketItemReceived").expect("OnMarketItemReceived emitted");
    assert_eq!(item_recv.args.last(), Some(&PyRep::Integer(AMARR_STATION_ID)));

    let _ = rpc(&ctx, &mut sess_amarr, "marketProxy", "GetCharOrders", &[]).await.expect("GetCharOrders second call");
    assert_eq!(ctx.db.get_character(P1).unwrap().balance.0, p1_bal_before_match);
}

#[tokio::test]
async fn test_e4_structure_industry_service_pause_and_many_jobs() {
    // parity: plan:E-4, feat:structures.service-industry
    eve_testkit::attest(&[
        "plan:E-4",
        "feat:structures.service-industry",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let struct_id = 1_000_000_201i64;
    let now_u = ctx.now_unix();

    // 1. Structure industry services: test Manufacturing (1), Research (3), and Invention (8) via InstallJob
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.corporation_id = CORP_ID;
            ch.balance = IskCents(100_000_000_00);
            ch.station_id = None;
            ch.structure_id = Some(struct_id);
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
        for &(tid, lvl) in &[(3380, 5), (3387, 5), (3402, 5), (3403, 5), (3406, 5), (11448, 5), (11452, 5), (21791, 5)] {
            snap.skills.entry(P1).or_default().insert(
                tid,
                TrainedSkillRecord {
                    type_id: tid,
                    level: lvl,
                    skill_points: 256_000.0,
                },
            );
        }
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35826, // Azbel (Engineering Complex)
                owner_corp_id: CORP_ID,
                solar_system_id: JITA_SYSTEM_ID,
                name: "Test Azbel".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 5_000,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        for (iid, is_bpc) in [(700_010, false), (700_011, false), (700_012, false), (700_013, true), (700_014, true)] {
            let mut bp = make_item(iid, 691, P1, struct_id, InventoryFlag::Hangar, 1, true);
            bp.is_blueprint_copy = is_bpc;
            bp.blueprint_runs_remaining = is_bpc.then_some(10);
            snap.items.insert(iid, bp);
        }
        for (iid, tid, qty) in [
            (700_020, 34u32, 5_000_000),
            (700_021, 35u32, 1_000_000),
            (700_022, 36u32, 500_000),
            (700_023, 37u32, 500_000),
            (700_024, 20424u32, 100),
            (700_025, 20172u32, 100),
        ] {
            snap.items.insert(iid, make_item(iid, tid, P1, struct_id, InventoryFlag::Hangar, qty, false));
        }
    });

    let mut sess = SessionState::new(8401);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)])
        .await
        .expect("SelectCharacterID");

    // 1a. With ZERO service modules fitted on the Upwell structure, InstallJob refuses all industry activities (R-IND-NO-MODULES)
    for act_id in [1i64, 3i64, 5i64, 8i64] {
        let err = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(691), PyRep::Integer(act_id), PyRep::Integer(1)])
            .await
            .expect_err("InstallJob must fail when Upwell structure has zero service modules fitted");
        assert_eq!(err.code, "FacilityServiceOffline");
    }

    // 1b. Fit Standup Manufacturing Plant I (35878), Standup Research Lab I (35891), Standup Invention Lab I (35886) initially OFFLINE
    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, flag) in [
            (struct_id + 1, 35878u32, InventoryFlag::ServiceSlot0),
            (struct_id + 2, 35891u32, InventoryFlag::ServiceSlot1),
            (struct_id + 3, 35886u32, InventoryFlag::ServiceSlot2),
        ] {
            let mut svc = make_item(iid, tid, CORP_ID, struct_id, flag, 1, true);
            svc.dynamic_attributes.insert(2, 0.0);
            snap.items.insert(iid, svc);
            snap.module_online_states.insert(iid, false);
        }
    });
    for act_id in [1i64, 3i64, 5i64, 8i64] {
        let err = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(691), PyRep::Integer(act_id), PyRep::Integer(1)])
            .await
            .expect_err("InstallJob must fail when activity's Standup service module is offline");
        assert_eq!(err.code, "FacilityServiceOffline");
    }

    // Online only Standup Manufacturing Plant I (35878): Manufacturing (1) succeeds, while Research (3), Copying (5), & Invention (8) still fail
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&(struct_id + 1)) {
            it.dynamic_attributes.insert(2, 1.0);
        }
        snap.module_online_states.insert(struct_id + 1, true);
    });
    let mfg_job = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(691), PyRep::Integer(1), PyRep::Integer(1)])
        .await
        .expect("Manufacturing InstallJob succeeds when Standup Manufacturing Plant I is online");
    assert!(mfg_job.dict_get("jobID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    for act_id in [3i64, 5i64, 8i64] {
        let err = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(691), PyRep::Integer(act_id), PyRep::Integer(1)])
            .await
            .expect_err("Research/Copying/Invention must still fail while only Manufacturing service is online");
        assert_eq!(err.code, "FacilityServiceOffline");
    }

    // Online Standup Research Lab I (35891) and Standup Invention Lab I (35886): ME Research (3), Copying (5), and Invention (8) now succeed!
    ctx.db.with_mut_snapshot(|snap| {
        for iid in [struct_id + 2, struct_id + 3] {
            if let Some(it) = snap.items.get_mut(&iid) {
                it.dynamic_attributes.insert(2, 1.0);
            }
            snap.module_online_states.insert(iid, true);
        }
    });
    for act_id in [3i64, 5i64, 8i64] {
        let job_rep = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(691), PyRep::Integer(act_id), PyRep::Integer(1)])
            .await
            .expect("Research/Copying/Invention InstallJob succeeds when corresponding Standup lab is online");
        assert!(job_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    }

    // 2. Pause timing: initialize structure fuel lifecycle at now_u, then leave 1 fuel block so services
    // run out of fuel and go offline at the first hourly fuel tick (now_u + 3_600).
    eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&struct_id) {
            st.fuel_blocks = 1;
        }
    });

    ctx.db.with_mut_snapshot(|snap| {
        for bp_id in [700_001i64, 700_004i64, 700_002i64] {
            snap.items.insert(bp_id, make_item(bp_id, 681, P1, struct_id, InventoryFlag::Hangar, 1, true));
        }
        for (idx, m_tid) in (34_u32..=40_u32).enumerate() {
            let iid = 700_100 + idx as i64;
            snap.items.insert(iid, make_item(iid, m_tid, P1, struct_id, InventoryFlag::Hangar, 100_000, false));
        }
    });
    let j1_rep = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(700_001)), ("facilityID", PyRep::Integer(struct_id)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
    ])])
        .await
        .expect("InstallJob 1");
    let job_done_before = j1_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let j2_rep = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(700_004)), ("facilityID", PyRep::Integer(struct_id)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
    ])])
        .await
        .expect("InstallJob 2");
    let job_done_before_2 = j2_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let j3_rep = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(700_002)), ("facilityID", PyRep::Integer(struct_id)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(10)),
    ])])
        .await
        .expect("InstallJob 3 (long)");
    let job_active_during = j3_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let orig_dur_3 = ctx.db.with_snapshot(|snap| snap.industry_jobs.get(&job_active_during).unwrap().duration_seconds);

    // Advance clock to now_u + 7_200 (2 hours later; fuel ran out at now_u + 3_600)
    ctx.advance_secs(7_200);
    eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);

    let _ = rpc(&ctx, &mut sess, "industryManager", "GetJobsByOwner", &[PyRep::Integer(P1)])
        .await
        .expect("GetJobsByOwner");

    ctx.db.with_snapshot(|snap| {
        assert!(!is_structure_industry_service_online(snap, struct_id, 1));
        assert_eq!(snap.industry_jobs.get(&job_done_before).unwrap().status, "Active");
        assert_eq!(snap.industry_jobs.get(&job_active_during).unwrap().status, "Paused");
        assert_eq!(
            load_job_cost_meta(snap, job_active_during).unwrap().paused_at_unix,
            Some(now_u + 3_600),
            "Pause starts at exact fuel exhaustion moment (now_u + 3_600), not first read (now_u + 7_200)"
        );
    });

    // Refuel and re-online service modules at now_u + 7_200; verify paused duration (+3,600s) is added on resume
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&struct_id) {
            st.fuel_blocks = 5_000;
        }
        for iid in [struct_id + 1, struct_id + 2, struct_id + 3] {
            if let Some(it) = snap.items.get_mut(&iid) {
                it.dynamic_attributes.insert(2, 1.0);
            }
            snap.module_online_states.insert(iid, true);
        }
    });
    eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);
    let _ = rpc(&ctx, &mut sess, "industryManager", "GetJobsByOwner", &[PyRep::Integer(P1)])
        .await
        .expect("GetJobsByOwner after refuel");
    ctx.db.with_snapshot(|snap| {
        let j2 = snap.industry_jobs.get(&job_active_during).unwrap();
        assert_eq!(j2.status, "Active");
        assert_eq!(j2.duration_seconds, orig_dur_3 + 3_600);
    });

    // 3. CompleteManyJobs delivers all ready jobs
    let many_rep = rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "CompleteManyJobs",
        &[PyRep::List(vec![PyRep::Integer(job_done_before), PyRep::Integer(job_done_before_2)])],
    )
    .await
    .expect("CompleteManyJobs delivers ready jobs");
    let many_str = format!("{many_rep:?}");
    assert!(many_str.contains(&job_done_before.to_string()) && many_str.contains(&job_done_before_2.to_string()));

    // 4. Ex-members cannot cancel or deliver corp jobs
    let corp_job_id = 880_003i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.insert(corp_job_id, make_job(corp_job_id, JITA_STATION_ID, 700_003, 50_000, now_u));
        let mut meta = IndustryJobCostMeta::default();
        meta.use_corp = true;
        meta.owner_id = CORP_ID;
        save_job_cost_meta(snap, corp_job_id, &meta);
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.corporation_id = 9999;
        }
    });

    let cancel_err = rpc(&ctx, &mut sess, "industryManager", "CancelJob", &[PyRep::Integer(corp_job_id)])
        .await
        .expect_err("Ex-member must not cancel corp job");
    assert_eq!(cancel_err.code, "JobOwnershipViolation");

    let deliver_err = rpc(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(corp_job_id)])
        .await
        .expect_err("Ex-member must not deliver corp job");
    assert_eq!(deliver_err.code, "JobOwnershipViolation");
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod market_escrow_settlement {
    use super::*;

//  Integration tests closing canonical plan row E-6
//  and its blocking planetary interaction lifecycle features.


use crate::common;
use eve_db::{CorporationRecord, IskCents, PlanetaryColonyRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::industry_pi::*;
use std::collections::HashMap;

const JITA_SYSTEM_ID: u32 = 30_000_142;
const JITA_STATION_ID: i64 = 60_003_760;
const P1: i64 = 90_000_001;
const POCO_CORP_ID: i64 = 2002;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

fn seed_extractor_colony(
    snap: &mut eve_db::WorldStateSnapshot,
    planet_id: i64,
    ecu_pin_id: i64,
    store_pin_id: i64,
    now_u: u64,
    expiry_unix: Option<u64>,
) {
    snap.planetary_colonies.insert(
        (P1, planet_id),
        PlanetaryColonyRecord {
            planet_id,
            owner_id: P1,
            solar_system_id: JITA_SYSTEM_ID,
            command_center_level: 3,
            pins_count: 2,
            last_extracted_unix: now_u,
            extracted_commodities: HashMap::new(),
        },
    );
    let net = PiColonyNetworkState {
        pins: vec![
            PiPinState {
                pin_id: ecu_pin_id,
                type_id: 2848, // Extractor Control Unit
                latitude: 0.1,
                longitude: 0.1,
                extractor_resource_type_id: Some(2268), // Aqueous Liquids (0.01 m3/unit)
                extractor_head_count: 4,
                extractor_head_radius: 0.03,
                extractor_cycle_seconds: 900,
                program_install_unix: now_u,
                expiry_time_unix: expiry_unix,
                ..Default::default()
            },
            PiPinState {
                pin_id: store_pin_id,
                type_id: 2536, // Storage Facility (12,000 m3 capacity)
                latitude: 0.2,
                longitude: 0.2,
                ..Default::default()
            },
        ],
        links: vec![PiLinkState {
            source_pin_id: ecu_pin_id,
            dest_pin_id: store_pin_id,
            level: 1,
            ..Default::default()
        }],
        routes: vec![PiRouteState {
            route_id: 1,
            commodity_type_id: 2268,
            quantity: 100,
            path: vec![ecu_pin_id, store_pin_id],
        }],
        last_simulated_unix: now_u,
        ..Default::default()
    };
    save_pi_network(snap, P1, planet_id, &net);
}

#[tokio::test]
async fn test_e6_pi_extractor_expiry_and_launch_canister() {
    // parity: plan:E-6, feat:lifecycle.pi-extractor-program-expiry, feat:lifecycle.pi-launch-canister
    eve_testkit::attest(&[
        "plan:E-6",
        "feat:lifecycle.pi-extractor-program-expiry",
        "feat:lifecycle.pi-launch-canister",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let planet_single = 40_009_082i64; // Jita IV (single-call colony)
    let planet_multi = 40_009_083i64;  // Jita V (repeated-call colony)
    let ecu_pin_id = 50_000_001i64;
    let store_pin_id = 50_000_002i64;
    let now_u = ctx.now_unix();

    // 1. Seed two identical colonies for RPC-level repeated-call invariance (GetColony / StartTicking)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.balance = IskCents(50_000_000_00);
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
        let mut poco_corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = POCO_CORP_ID;
            __corp.name = "Orbital Customs Corp".into();
            __corp.ticker = "POCO".into();
            __corp.ceo_id = 90_000_002;
            __corp.alliance_id = None;
            __corp.tax_rate = 0.05;
            __corp.description = String::new();
            __corp.url = String::new();
            __corp.hq_station_id = Some(JITA_STATION_ID);
            __corp.members = [90_000_002].into_iter().collect();
            __corp.member_roles = HashMap::new();
            __corp.applications = HashMap::new();
            __corp.wallet_divisions = Vec::new();
            let __mw = IskCents(1_000_000_00);
            if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); }
            __corp };
        poco_corp.ensure_wallet_divisions();
        poco_corp.wallet_divisions[0].balance = IskCents(1_000_000_00);
        snap.corporations.insert(POCO_CORP_ID, poco_corp);
        save_orbital_settings(
            snap,
            planet_single,
            &OrbitalCustomsSettings {
                planet_id: planet_single,
                owner_corp_id: POCO_CORP_ID,
                corporation_tax_rate: 0.08,
                alliance_tax_rate: 0.08,
                standing_neutral_tax_rate: 0.12,
                allow_alliance: true,
                allow_standings: true,
                minimum_standing: -10.0,
                ..Default::default()
            },
        );
        seed_extractor_colony(snap, planet_single, ecu_pin_id, store_pin_id, now_u, Some(now_u + 7_200));
        seed_extractor_colony(snap, planet_multi, ecu_pin_id, store_pin_id, now_u, Some(now_u + 7_200));
    });

    let mut sess = SessionState::new(8501);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)])
        .await
        .expect("SelectCharacterID");
    sess.solar_system_id = Some(JITA_SYSTEM_ID);

    // 1a. Drive planet_multi through 16 sub-cycle steps of 450s via planetMgr.StartTicking & planetMgr.GetColony RPCs,
    // then drive planet_single through a single 7,200s planetMgr.GetColony RPC call at the end.
    for step in 1..=16 {
        ctx.advance_secs(450);
        if step % 2 == 1 {
            rpc(&ctx, &mut sess, "planetMgr", "StartTicking", &[PyRep::Integer(planet_multi)])
                .await
                .expect("planetMgr.StartTicking");
        } else {
            rpc(&ctx, &mut sess, "planetMgr", "GetColony", &[PyRep::Integer(planet_multi)])
                .await
                .expect("planetMgr.GetColony");
        }
    }
    let _ = rpc(&ctx, &mut sess, "planetMgr", "GetColony", &[PyRep::Integer(planet_multi)])
        .await
        .expect("GetColony multi final");
    let single_rep = rpc(&ctx, &mut sess, "planetMgr", "GetColony", &[PyRep::Integer(planet_single)])
        .await
        .expect("GetColony single 7,200s");
    assert!(
        single_rep.dict_get("simulatedExtractedQuantity").and_then(|v| v.as_i64()).unwrap_or(0) > 0,
        "Single 7,200s GetColony call reports positive simulatedExtractedQuantity"
    );

    let (single_yield, multi_yield) = ctx.db.with_snapshot(|snap| {
        let net_s = load_pi_network(snap, P1, planet_single, 3);
        let net_m = load_pi_network(snap, P1, planet_multi, 3);
        (
            *net_s.pins[1].contents.get(&2268).unwrap_or(&0),
            *net_m.pins[1].contents.get(&2268).unwrap_or(&0),
        )
    });
    assert!(single_yield > 0, "Extractor produced yield");
    assert_eq!(
        single_yield, multi_yield,
        "RPC-level yield invariance: 16 StartTicking/GetColony sub-cycle calls == 1 single 7,200s GetColony call"
    );

    // Repeated GetColony/StartTicking at the same timestamp (0s elapsed) produces 0 extra yield
    let zero_rep = rpc(&ctx, &mut sess, "planetMgr", "GetColony", &[PyRep::Integer(planet_single)])
        .await
        .expect("GetColony with 0s elapsed");
    assert_eq!(
        zero_rep.dict_get("simulatedExtractedQuantity").and_then(|v| v.as_i64()),
        Some(0),
        "Repeated GetColony with 0s elapsed yields 0"
    );

    // 1b. Extractor stops producing at install + duration (expiry); Flush after expiry never mints commodities
    ctx.advance_secs(10_000);
    rpc(&ctx, &mut sess, "planetMgr", "StartTicking", &[PyRep::Integer(planet_single)])
        .await
        .expect("StartTicking past expiry");
    let post_expiry_yield = ctx.db.with_snapshot(|snap| {
        *load_pi_network(snap, P1, planet_single, 3).pins[1].contents.get(&2268).unwrap_or(&0)
    });
    assert_eq!(single_yield, post_expiry_yield, "Extractor stops producing past expiry");

    ctx.advance_secs(900);
    let flush_expired = rpc(&ctx, &mut sess, "planetMgr", "Flush", &[PyRep::Integer(planet_single)])
        .await
        .expect("Flush after extractor expiry");
    assert_eq!(
        flush_expired.dict_get("producedQuantity").and_then(|v| v.as_i64()),
        Some(0),
        "Flush must produce 0 when all extractor programs on the planet have expired"
    );

    // 1c. Bounded program duration on UserUpdateNetwork command 10 (InstallProgram):
    // Default (no client duration) -> 86,400s (24h); excessive client duration (99,999,999s) -> clamped to 14 * 86,400s (1,209,600s)
    let install_now = ctx.now_unix();
    rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(planet_single),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(10),
                PyRep::Integer(ecu_pin_id),
                PyRep::Integer(2268),
                PyRep::Real(0.03),
            ])]),
        ],
    )
    .await
    .expect("UserUpdateNetwork command 10 default duration");
    let default_exp = ctx.db.with_snapshot(|snap| {
        load_pi_network(snap, P1, planet_single, 3).pins[0].expiry_time_unix
    });
    assert_eq!(
        default_exp,
        Some(install_now + 86_400),
        "Extractor program installed without explicit duration defaults to bounded 24h (86,400s)"
    );

    rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(planet_single),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(10),
                PyRep::Integer(ecu_pin_id),
                PyRep::Integer(2268),
                PyRep::Real(0.03),
                PyRep::Integer(4),
                PyRep::Integer(900),
                PyRep::Integer((install_now + 99_999_999) as i64),
            ])]),
        ],
    )
    .await
    .expect("UserUpdateNetwork command 10 clamped duration");
    let clamped_exp = ctx.db.with_snapshot(|snap| {
        load_pi_network(snap, P1, planet_single, 3).pins[0].expiry_time_unix
    });
    assert_eq!(
        clamped_exp,
        Some(install_now + 14 * 86_400),
        "Client-supplied extractor expiry is clamped to 14 days (1,209,600s) max"
    );

    // Calling Flush immediately after install (before 1 cycle elapses) is rejected with ExtractorCycleNotReady
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(col) = snap.planetary_colonies.get_mut(&(P1, planet_single)) {
            col.last_extracted_unix = install_now;
        }
    });
    let flush_early_err = rpc(&ctx, &mut sess, "planetMgr", "Flush", &[PyRep::Integer(planet_single)])
        .await
        .expect_err("Flush before cycle elapses must fail");
    assert_eq!(flush_early_err.code, "ExtractorCycleNotReady");

    // 2. Active extractor output beyond storage capacity is lost AND offline owner receives
    // PlanetExtractorFinished + PlanetStorageFull notifications on next login (SelectCharacterID)
    let offline_now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(col) = snap.planetary_colonies.get_mut(&(P1, planet_single)) {
            col.last_extracted_unix = offline_now;
        }
        let mut net = load_pi_network(snap, P1, planet_single, 3);
        net.pins[0].program_install_unix = offline_now;
        net.pins[0].extractor_cycle_seconds = 900;
        net.pins[0].expiry_time_unix = Some(offline_now + 1_800);
        // Storage Facility (2536) has capacity 12,000.0 m3; Aqueous Liquids (2268) is 0.01 m3/unit -> max 1,200,000 units.
        // Pre-fill with 1,199,900 units so the first 900s cycle fills storage to 1,200,000 and overflows.
        net.pins[1].contents.clear();
        net.pins[1].contents.insert(2268, 1_199_900);
        net.last_simulated_unix = offline_now;
        save_pi_network(snap, P1, planet_single, &net);
    });

    let _ = ctx.drain_notifications_for_char(P1);
    // Advance 3,600s while P1 is offline (extractor finishes at +1,800s and storage fills to capacity)
    ctx.advance_secs(3_600);

    let mut sess_relogin = SessionState::new(8502);
    common::login_pilot(&ctx, &mut sess_relogin, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess_relogin, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)])
        .await
        .expect("SelectCharacterID after offline PI events");
    sess_relogin.solar_system_id = Some(JITA_SYSTEM_ID);

    let login_notifs = ctx.drain_notifications_for_char(P1);
    assert!(
        login_notifs.iter().any(|n| n.event == "PlanetExtractorFinished"),
        "Offline owner receives PlanetExtractorFinished notification at login"
    );
    assert!(
        login_notifs.iter().any(|n| n.event == "PlanetStorageFull"),
        "Offline owner receives PlanetStorageFull notification at login"
    );
    let persisted_types: Vec<i32> = ctx
        .notification_records
        .read()
        .unwrap()
        .iter()
        .filter(|r| r.receiver_id == P1)
        .map(|r| r.type_id)
        .collect();
    assert!(
        persisted_types.contains(&101) && persisted_types.contains(&102),
        "Both PlanetExtractorFinished (101) and PlanetStorageFull (102) persisted in notification_records"
    );

    // Trigger GetColony after relogin: verify storage clamped at 1,200,000 units and NO duplicate 101/102 notifications emitted
    let _ = rpc(&ctx, &mut sess_relogin, "planetMgr", "GetColony", &[PyRep::Integer(planet_single)])
        .await
        .expect("GetColony after relogin");
    let post_get_colony_notifs = ctx.drain_notifications_for_char(P1);
    assert!(
        !post_get_colony_notifs
            .iter()
            .any(|n| n.event == "PlanetExtractorFinished" || n.event == "PlanetStorageFull"),
        "GetColony after login sync must not re-fire duplicate PlanetExtractorFinished or PlanetStorageFull notifications"
    );
    let capped_qty = ctx.db.with_snapshot(|snap| {
        *load_pi_network(snap, P1, planet_single, 3).pins[1].contents.get(&2268).unwrap_or(&0)
    });
    assert_eq!(
        capped_qty, 1_200_000,
        "Active extractor output beyond storage capacity (12,000 m3 = 1,200,000 units) is lost"
    );

    // 2b. Processor schematic (3,000 Aqueous Liquids P0 -> 20 Water P1) and expedited transfer cooldown
    let proc_client_id = 50_000_003i64;
    let lp_client_id = 50_000_004i64;
    rpc(
        &ctx,
        &mut sess_relogin,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(planet_single),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(proc_client_id), PyRep::Integer(2473), PyRep::Real(0.25), PyRep::Real(0.25)]),
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(lp_client_id), PyRep::Integer(2544), PyRep::Real(0.30), PyRep::Real(0.30)]),
                PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(store_pin_id), PyRep::Integer(proc_client_id), PyRep::Integer(1)]),
                PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(proc_client_id), PyRep::Integer(lp_client_id), PyRep::Integer(1)]),
                PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(proc_client_id), PyRep::Integer(121)]),
            ]),
        ],
    )
    .await
    .expect("UserUpdateNetwork add processor, launchpad, links, and Water schematic 121");
    let (proc_pin_id, lp_pin_id) = ctx.db.with_snapshot(|snap| {
        let net = load_pi_network(snap, P1, planet_single, 3);
        let p_id = net.pins.iter().find(|p| p.type_id == 2473).unwrap().pin_id;
        let l_id = net.pins.iter().find(|p| p.type_id == 2544).unwrap().pin_id;
        (p_id, l_id)
    });
    rpc(
        &ctx,
        &mut sess_relogin,
        "planetMgr",
        "UserTransferCommodities",
        &[
            PyRep::Integer(planet_single),
            PyRep::List(vec![PyRep::Integer(store_pin_id), PyRep::Integer(proc_pin_id), PyRep::Integer(lp_pin_id)]),
            PyRep::Dict(vec![(PyRep::Integer(2268), PyRep::Integer(500))]),
        ],
    )
    .await
    .expect("UserTransferCommodities expedited transfer");
    let cd_err = rpc(
        &ctx,
        &mut sess_relogin,
        "planetMgr",
        "UserTransferCommodities",
        &[
            PyRep::Integer(planet_single),
            PyRep::List(vec![PyRep::Integer(store_pin_id), PyRep::Integer(proc_pin_id), PyRep::Integer(lp_pin_id)]),
            PyRep::Dict(vec![(PyRep::Integer(2268), PyRep::Integer(100))]),
        ],
    )
    .await
    .expect_err("Expedited transfer within cooldown must fail");
    assert_eq!(cd_err.code, "ExpeditedTransferOnCooldown");

    // 3. POCO export tax charged & journaled on CustomsOffice export, and Launch Canister lifecycle (DeleteLaunch + 5-day expiry)
    ctx.db.with_mut_snapshot(|snap| {
        let mut net = load_pi_network(snap, P1, planet_single, 3);
        net.pins[1].contents.clear();
        net.pins[1].contents.insert(3645, 1_000); // 1,000 units of Water (P1 commodity, base 400 ISK)
        save_pi_network(snap, P1, planet_single, &net);
    });

    let p1_bal_before_poco = ctx.db.get_character(P1).unwrap().balance.0;
    let poco_corp_before = ctx.db.with_snapshot(|s| s.corporations[&POCO_CORP_ID].master_wallet_balance().0);
    let poco_export_rep = rpc(
        &ctx,
        &mut sess_relogin,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(planet_single),
            PyRep::Integer(3645),
            PyRep::Integer(200),
            PyRep::String("CustomsOffice".into()),
        ],
    )
    .await
    .expect("UserLaunchCommodities via CustomsOffice");
    let export_tax_isk = poco_export_rep
        .dict_get("exportTaxISK")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!(export_tax_isk > 0.0, "POCO export must charge positive exportTaxISK");
    let p1_bal_after_poco = ctx.db.get_character(P1).unwrap().balance.0;
    let poco_corp_after = ctx.db.with_snapshot(|s| s.corporations[&POCO_CORP_ID].master_wallet_balance().0);
    assert!(p1_bal_after_poco < p1_bal_before_poco, "P1 wallet debited by POCO export tax");
    assert!(poco_corp_after > poco_corp_before, "POCO owner corporation credited with player tax share");
    let poco_launch_id = poco_export_rep.dict_get("launchID").and_then(|v| v.as_i64()).expect("poco_launch_id");

    // DeleteLaunch on a REAL existing launch ID succeeds and removes the launch canister and its ball
    let del_ok = rpc(&ctx, &mut sess_relogin, "planetMgr", "DeleteLaunch", &[PyRep::Integer(poco_launch_id)])
        .await
        .expect("DeleteLaunch on real launch canister must succeed");
    assert_eq!(del_ok, PyRep::Bool(true));
    assert!(
        ctx.db.with_snapshot(|s| !s.items.contains_key(&poco_launch_id)),
        "DeleteLaunch removes the launch canister item from space"
    );

    // DeleteLaunch on a non-launch item ID is rejected with InvalidLaunchID
    let del_err = rpc(&ctx, &mut sess_relogin, "planetMgr", "DeleteLaunch", &[PyRep::Integer(100_000_001)])
        .await
        .expect_err("DeleteLaunch must reject non-launch ID");
    assert_eq!(del_err.code, "InvalidLaunchID");

    // Command Center launch creates a real Planetary Launch Container item and Ball in space near the planet
    // (never in station hangar), places commodities inside the container, preserves launchTime, and expires after 5 days
    let launch_ts = ctx.now_unix() as i64;
    let launch_rep = rpc(
        &ctx,
        &mut sess_relogin,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(planet_single),
            PyRep::Integer(store_pin_id),
            PyRep::Integer(3645),
            PyRep::Integer(500),
        ],
    )
    .await
    .expect("UserLaunchCommodities CommandCenterLaunch");
    let launch_id = launch_rep
        .as_i64()
        .or_else(|| launch_rep.dict_get("launchID").and_then(|v| v.as_i64()))
        .expect("launch_id");
    assert!(launch_id > 0);
    let (container_item, inner_commodity) = ctx.db.with_snapshot(|s| {
        let cont = s.items.get(&launch_id).cloned().expect("Planetary Launch Container item");
        let inner = s
            .items
            .values()
            .find(|i| i.location_id == launch_id)
            .cloned()
            .expect("Commodity item inside launch container");
        (cont, inner)
    });
    assert_eq!(
        container_item.location_id,
        JITA_SYSTEM_ID as i64,
        "Planetary Launch Container located in solar system space"
    );
    assert_ne!(
        container_item.type_id, 3645,
        "Launch item is a Planetary Launch Container SDE type, not the raw commodity"
    );
    assert_eq!(inner_commodity.type_id, 3645);
    assert_eq!(inner_commodity.quantity, 500);
    let sys_handle = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);
    let (_, launch_ball) = sys_handle
        .get_ball_snapshot(launch_id)
        .await
        .expect("ball query")
        .expect("Planetary Launch Container ball spawned in ballpark");
    assert_eq!(launch_ball.type_id, container_item.type_id);
    assert!(
        launch_ball.position.length() > 1_000.0,
        "Launch container ball has non-zero orbital coordinates near planet"
    );
    let in_hangar = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|i| i.location_id == JITA_STATION_ID && i.type_id == 3645)
    });
    assert!(!in_hangar, "Never places launched goods in station hangar");

    // Advance 60s and verify GetMyLaunchesDetails reports the stored launchTime and non-zero (x, y, z)
    ctx.advance_secs(60);
    let my_launches = rpc(&ctx, &mut sess_relogin, "planetMgr", "GetMyLaunchesDetails", &[PyRep::Integer(planet_single)])
        .await
        .expect("GetMyLaunchesDetails");
    let PyRep::List(ref launch_list) = my_launches else {
        panic!("GetMyLaunchesDetails must return List");
    };
    let found_launch = launch_list
        .iter()
        .find(|entry| entry.dict_get("launchID").and_then(|v| v.as_i64()) == Some(launch_id))
        .expect("launch entry in GetMyLaunchesDetails");
    assert_eq!(
        found_launch.dict_get("launchTime").and_then(|v| v.as_i64()),
        Some(launch_ts),
        "GetMyLaunchesDetails reports stored launchTime"
    );
    let lx = found_launch.dict_get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let ly = found_launch.dict_get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let lz = found_launch.dict_get("z").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(
        lx.abs() > 1.0 && ly.abs() > 1.0 && lz.abs() > 1.0,
        "GetMyLaunchesDetails reports non-zero orbital coordinates (x, y, z)"
    );

    // Uncollected after 5 days is removed by background tick (before GetMyLaunchesDetails is called)
    ctx.advance_secs(5 * 86_400 + 100);
    assert!(
        ctx.db.with_snapshot(|s| !s.items.contains_key(&launch_id) && !s.items.contains_key(&inner_commodity.item_id)),
        "Background tick removes expired planetary launch container and its inner commodities after 5 days"
    );
    assert!(
        sys_handle.get_ball_snapshot(launch_id).await.expect("ball query").is_none(),
        "Background tick removes expired planetary launch container ball from ballpark"
    );
    let launches_expired = rpc(&ctx, &mut sess_relogin, "planetMgr", "GetMyLaunchesDetails", &[PyRep::Integer(planet_single)])
        .await
        .expect("GetMyLaunchesDetails after 5 days");
    assert!(
        !format!("{launches_expired:?}").contains(&launch_id.to_string()),
        "Uncollected launch removed after 5 days"
    );
}

}
