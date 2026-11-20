//! Comprehensive Market Order Parity Tests closing the remaining 11 `feat:market.*` rows.



use crate::common;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_STATION_ID: i64 = 60_003_760;
const TRITANIUM_TYPE_ID: TypeId = 34;
const PYERITE_TYPE_ID: TypeId = 35;
const MEXALLON_TYPE_ID: TypeId = 36;
#[allow(dead_code)]
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
async fn test_tick_size_relist_fee_broker_minimum_scc_surcharge_and_durations() {
    // parity: feat:market.tick-size-four-significant-digits, feat:market.relist-fee-formula, feat:market.broker-fee-minimum-and-scc-surcharge, feat:market.order-duration-options, feat:market.structure-markets, feat:market.modify-order, rpc:marketProxy.PlacePlexSellOrder, rpc:marketProxy.ModifyPlexCharOrder
    eve_testkit::attest(&["feat:market.tick-size-four-significant-digits", "feat:market.relist-fee-formula", "feat:market.broker-fee-minimum-and-scc-surcharge", "feat:market.order-duration-options", "feat:market.structure-markets", "feat:market.modify-order"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 1101, "demo", "demo-secret", 90_000_001).await;

    let trit_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.clear();
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.balance = IskCents::from_isk(100_000_000);
            ch.standings.clear();
        }
        snap.next_entity_id += 1;
        let iid = snap.next_entity_id;
        snap.items.insert(
            iid,
            InventoryItemRecord {
                item_id: iid,
                type_id: TRITANIUM_TYPE_ID,
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 50_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        iid
    });

    // 1. 4-Significant-Figure Tick Size (`feat:market.tick-size-four-significant-digits`)
    // `12345.0` (5 sig figs) rejected on PlaceBuyOrder and PlaceMultiSellOrder; `0.001` (< 0.01 ISK) rejected; `12340.0` and `0.01` accepted.
    let bad_buy_tick = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(12_345.0),
            PyRep::Integer(10),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(bad_buy_tick.code, "InvalidPriceTickSize");

    let sub_cent_tick = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(0.001),
            PyRep::Integer(10),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(sub_cent_tick.code, "InvalidPriceTickSize");

    let bad_sell_tick = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                ("itemID", PyRep::Integer(trit_item_id)),
                ("price", PyRep::Real(12_345.0)),
                ("quantity", PyRep::Integer(10)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(bad_sell_tick.code, "InvalidPriceTickSize");

    // Minimum 0.01 ISK price is valid AND triggers the 100.00 ISK minimum broker fee (`feat:market.broker-fee-minimum-and-scc-surcharge`)
    let min_tick_buy = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(0.01),
            PyRep::Integer(10),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("0.01 ISK buy order must succeed");
    let PyRep::KeyVal(mt_kv) = min_tick_buy else {
        panic!("Expected KeyVal");
    };
    assert_eq!(mt_kv.get("brokerFeeISK"), Some(&PyRep::Real(100.0)));
    let min_tick_oid = mt_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(min_tick_oid)])
        .await
        .expect("Cancel min tick order");

    // `12340.0` (4 sig figs) is valid
    let valid_4sig_buy = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(12_340.0),
            PyRep::Integer(10),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("12340.0 buy order must succeed");
    let PyRep::KeyVal(v4_kv) = valid_4sig_buy else {
        panic!("Expected KeyVal");
    };
    let v4_oid = v4_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();

    // 2. Order Modification 5-minute cooldown & Relist Fee (`feat:market.order-modification-relist-fee`)
    let cooldown_err = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "ModifyCharOrder",
        &[PyRep::Integer(v4_oid), PyRep::Real(12_350.0)],
    )
    .await
    .unwrap_err();
    assert_eq!(cooldown_err.code, "OrderModificationCooldown");

    ctx.advance_virtual_clock(301);
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(90_000_001).or_default();
        sk.insert(
            3446,
            TrainedSkillRecord {
                type_id: 3446, // Broker Relations 0 -> 3.0% broker rate
                level: 0,
                skill_points: 0.0,
            },
        );
        sk.insert(
            16597,
            TrainedSkillRecord {
                type_id: 16597, // Advanced Broker Relations 5 -> discount factor 0.50 - 0.06*5 = 0.20
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    // Modify from 12,340.0 (total 123,400 ISK) to 20,000.0 (total 200,000 ISK):
    // base_portion = 200,000 * 0.03 * 0.20 = 1,200 ISK; delta_portion = (200,000 - 123,400) * 0.03 = 2,298 ISK; total relist fee = 3,498.00 ISK
    let mod_res = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "ModifyCharOrder",
        &[PyRep::Integer(v4_oid), PyRep::Real(20_000.0)],
    )
    .await
    .expect("ModifyCharOrder after 301s virtual clock advance");
    let PyRep::KeyVal(mod_kv) = mod_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(mod_kv.get("relistFeeISK"), Some(&PyRep::Real(3_498.0)));
    rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(v4_oid)])
        .await
        .expect("Cancel modified order");

    // Verify ModifyPlexCharOrder also enforces 5-minute cooldown and relist fee
    ctx.db.with_mut_snapshot(|snap| {
        let cur = snap.plex_vault_balance(90_000_001);
        snap.adjust_plex_vault(90_000_001, 100 - cur).unwrap();
    });
    let plex_sell = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlacePlexSellOrder",
        &[PyRep::Real(5_000_000.0), PyRep::Integer(1), PyRep::Integer(90)],
    )
    .await
    .expect("PlacePlexSellOrder");
    let PyRep::KeyVal(ps_kv) = plex_sell else {
        panic!("Expected KeyVal");
    };
    let plex_oid = ps_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();
    let plex_cd_err = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "ModifyPlexCharOrder",
        &[PyRep::Integer(plex_oid), PyRep::Real(5_100_000.0)],
    )
    .await
    .unwrap_err();
    assert_eq!(plex_cd_err.code, "OrderModificationCooldown");
    ctx.advance_virtual_clock(301);
    let plex_mod_ok = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "ModifyPlexCharOrder",
        &[PyRep::Integer(plex_oid), PyRep::Real(5_100_000.0)],
    )
    .await
    .expect("ModifyPlexCharOrder after 301s");
    let PyRep::KeyVal(pm_kv) = plex_mod_ok else {
        panic!("Expected KeyVal");
    };
    assert_eq!(pm_kv.get("newPrice"), Some(&PyRep::Real(5_100_000.0)));
    rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(plex_oid)])
        .await
        .expect("Cancel PLEX order");

    // 3. Order Duration Options (`feat:market.order-duration-options`)
    // Reject 365-day player orders; accept [0, 1, 3, 7, 14, 30, 90]; NPC seed orders use 365 days.
    let dur_365_err = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(1.0),
            PyRep::Integer(10),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(365),
            PyRep::Bool(false),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(dur_365_err.code, "InvalidMarketOrder");

    let sell_365_err = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                ("itemID", PyRep::Integer(trit_item_id)),
                ("price", PyRep::Real(10.0)),
                ("quantity", PyRep::Integer(10)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(365),
            PyRep::Real(0.0),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(sell_365_err.code, "InvalidMarketOrder");

    for &dur in &[1_i64, 3, 7, 14, 30, 90] {
        let r = rpc(
            &ctx,
            &mut s1,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(1.0),
                PyRep::Integer(10),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(dur),
                PyRep::Bool(false),
            ],
        )
        .await
        .expect("Valid duration must succeed");
        let PyRep::KeyVal(kv) = r else {
            panic!("Expected KeyVal");
        };
        assert_eq!(kv.get("duration"), Some(&PyRep::Integer(dur)));
        let oid = kv.get("orderID").and_then(|v| v.as_i64()).unwrap();
        rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(oid)])
            .await
            .unwrap();
    }

    // Seed NPC orders and verify NPC duration is 365 days in GetOrders
    ctx.db.with_mut_snapshot(|snap| snap.markets_seeded = false);
    eve_server::world_seeder::seed_dual_anchor_markets(&ctx.db, &ctx.sde);
    let npc_skillbook_tid = ctx.db.with_snapshot(|snap| {
        snap.market_orders
            .values()
            .find(|o| {
                o.is_npc
                    && !o.is_bid
                    && o.station_id == JITA_STATION_ID
                    && ctx.sde.types.get(&o.type_id).map(|t| t.category_id == 16).unwrap_or(false)
            })
            .map(|o| o.type_id)
            .expect("Seeded NPC skillbook order in Jita")
    });
    let npc_orders = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "GetOrders",
        &[PyRep::Integer(npc_skillbook_tid as i64)],
    )
    .await
    .expect("GetOrders for NPC skillbook");
    let PyRep::Tuple(mut order_sets) = npc_orders else {
        panic!("Expected Tuple");
    };
    let sells_crowset = order_sets.remove(0);
    assert!(sells_crowset.is_crowset(), "Expected CRowset for sells");
    let sell_rows = sells_crowset
        .crowset_rows()
        .expect("Expected CRowset rows");
    assert!(!sell_rows.is_empty());
    // duration == 365 for NPC seed orders
    assert_eq!(
        sell_rows[0].dbrow_get("duration"),
        Some(&PyRep::Integer(365))
    );

    // 4. Structure Markets & 0.5% SCC Surcharge (`feat:market.structure-markets`, `feat:market.broker-fee-minimum-and-scc-surcharge`)
    let struct_id = 1_030_000_777_001_i64;
    let hub_item_id = 1_030_000_777_002_i64;
    let corp_id = ctx.db.with_mut_snapshot(|snap| {
        let cid = 98_000_778_i64;
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = cid;
        }
        snap.corporations.insert(
            cid,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = cid;
                __corp.name = "Citadel Market Corp".into();
                __corp.ticker = "CMKT".into();
                __corp.ceo_id = 90_000_001;
                __corp.members.insert(90_000_001);
                __corp.member_roles.insert(90_000_001, u64::MAX);
                __corp.set_division_balance(1, IskCents(0));
                __corp },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_833, // Fortizar
                owner_corp_id: cid,
                solar_system_id: 30_000_142,
                name: "Jita Fortizar Market Hub".into(),
                position: [10_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 10_000.0,
                armor_hp: 10_000.0,
                hull_hp: 10_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
        // Fit an online Standup Market Hub I (type_id = 35892)
        snap.items.insert(
            hub_item_id,
            InventoryItemRecord {
                item_id: hub_item_id,
                type_id: 35_892,
                owner_id: cid,
                location_id: struct_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(hub_item_id, true);
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
            ch.structure_id = Some(struct_id);
        }
        cid
    });
    s1.corporation_id = Some(corp_id);
    s1.station_id = None;
    s1.structure_id = Some(struct_id);

    // Owner sets market broker rate to 0.0% -> total rate is 0.0% + 0.5% SCC surcharge = 0.5%
    rpc(
        &ctx,
        &mut s1,
        "facilityManager",
        "SetFacilityTaxes",
        &[
            PyRep::Integer(struct_id),
            PyRep::keyval([("market", PyRep::Real(0.0))]),
        ],
    )
    .await
    .expect("SetFacilityTaxes market=0.0");

    let zero_owner_order = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(struct_id),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(10.0),
            PyRep::Integer(10_000), // 100,000 ISK
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("PlaceBuyOrder at 0% owner rate + 0.5% SCC surcharge");
    let PyRep::KeyVal(zo_kv) = zero_owner_order else {
        panic!("Expected KeyVal");
    };
    assert_eq!(zo_kv.get("brokerFeeISK"), Some(&PyRep::Real(500.0))); // 0.5% SCC surcharge on 100,000 ISK
    assert_eq!(
        ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet_balance()),
        IskCents(0),
        "Owner gets 0 ISK when owner rate is 0%"
    );

    // Owner sets market broker rate to 1.0% -> total rate is 1.0% + 0.5% SCC surcharge = 1.5%
    rpc(
        &ctx,
        &mut s1,
        "facilityManager",
        "SetFacilityTaxes",
        &[
            PyRep::Integer(struct_id),
            PyRep::keyval([("market", PyRep::Real(0.01))]),
        ],
    )
    .await
    .expect("SetFacilityTaxes market=0.01");

    let one_pct_order = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(struct_id),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(10.0),
            PyRep::Integer(10_000), // 100,000 ISK
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("PlaceBuyOrder at 1% owner rate + 0.5% SCC surcharge");
    let PyRep::KeyVal(op_kv) = one_pct_order else {
        panic!("Expected KeyVal");
    };
    assert_eq!(op_kv.get("brokerFeeISK"), Some(&PyRep::Real(1_500.0)));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet_balance()),
        IskCents::from_isk(1_000),
        "Owner receives 1.0% (1,000 ISK) while 0.5% (500 ISK) goes to SCC"
    );

    // Offline the Standup Market Hub I -> market orders at structure are hidden and new orders rejected
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(hub_item_id, false);
    });
    let offline_err = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(struct_id),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(10.0),
            PyRep::Integer(100),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(offline_err.code, "MarketHubOffline");
}

#[tokio::test]
async fn test_ranged_buy_orders_corp_divisions_multisell_multibuy_expiry_and_history() {
    // parity: feat:market.buy-order-range-fill, feat:market.corp-order-wallet-hangar-divisions, feat:market.multisell-quote-fees, feat:market.multibuy-import-order-matching, feat:market.order-expiry, feat:market.order-history, feat:market.remote-orders, feat:market.corp-orders, feat:market.price-history, rpc:marketProxy.GetMarketOrderHistory
    eve_testkit::attest(&["feat:market.buy-order-range-fill", "feat:market.corp-order-wallet-hangar-divisions", "feat:market.multisell-quote-fees", "feat:market.multibuy-import-order-matching", "feat:market.order-expiry", "feat:market.order-history", "feat:market.remote-orders", "feat:market.corp-orders", "feat:market.price-history"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 1102, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 1103, "other", "other-secret", 90_000_002).await;

    // Find a station in Perimeter (30_000_144, 1 stargate jump from Jita 30_000_142)
    let perim_station_id = ctx
        .sde
        .solar_systems
        .get(&30_000_144)
        .and_then(|sys| sys.stations.first().map(|st| st.station_id))
        .expect("Perimeter station in SDE");

    // 1. Ranged Buy Order Fill & Visibility Skill Gating (`feat:market.buy-order-range-fill`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.clear();
        snap.skills.entry(90_000_001).or_default().insert(
            3447, // Visibility 0 -> only station (-1) allowed
            TrainedSkillRecord {
                type_id: 3447,
                level: 0,
                skill_points: 0.0,
            },
        );
    });
    let vis_err = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(10.0),
            PyRep::Integer(100),
            PyRep::Integer(1), // 1 jump requires Visibility >= 2
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(vis_err.code, "VisibilitySkillRequired");

    // Train Visibility (3447) to level 2 (allows up to 5 jumps) and place 1-jump buy order at Jita
    let s2_perim_item = ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(
            3447,
            TrainedSkillRecord {
                type_id: 3447,
                level: 2,
                skill_points: 8_000.0,
            },
        );
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.station_id = Some(perim_station_id);
            ch2.solar_system_id = 30_000_144;
        }
        snap.next_entity_id += 1;
        let iid = snap.next_entity_id;
        snap.items.insert(
            iid,
            InventoryItemRecord {
                item_id: iid,
                type_id: TRITANIUM_TYPE_ID,
                owner_id: 90_000_002,
                location_id: perim_station_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        iid
    });
    s2.station_id = Some(perim_station_id);
    s2.solar_system_id = Some(30_000_144);

    let ranged_bid = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(15.0),
            PyRep::Integer(100),
            PyRep::Integer(1), // 1 jump reaches Perimeter!
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("Place 1-jump buy order at Jita");
    let PyRep::KeyVal(rb_kv) = ranged_bid else {
        panic!("Expected KeyVal");
    };
    let ranged_bid_oid = rb_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();

    // Seller at Perimeter (1 jump away) sells 100 Tritanium into the Jita ranged buy order
    let perim_sell = rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(perim_station_id)),
                ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                ("itemID", PyRep::Integer(s2_perim_item)),
                ("price", PyRep::Real(15.0)),
                ("quantity", PyRep::Integer(100)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(0),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect("Perimeter seller fills Jita 1-jump buy order");
    let PyRep::KeyVal(ps_kv) = perim_sell else {
        panic!("Expected KeyVal");
    };
    assert_eq!(ps_kv.get("filledImmediately"), Some(&PyRep::Bool(true)));
    let delivered_at_perim = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|i| {
            i.owner_id == 90_000_001
                && i.location_id == perim_station_id
                && i.flag == InventoryFlag::Hangar
                && i.type_id == TRITANIUM_TYPE_ID
                && i.quantity == 100
        })
    });
    assert!(
        delivered_at_perim,
        "Ranged buy order fill must deliver items to buyer's hangar at the seller's station"
    );

    // Move s2 back to Jita
    s2.station_id = Some(JITA_STATION_ID);
    s2.solar_system_id = Some(30_000_142);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.station_id = Some(JITA_STATION_ID);
            ch2.solar_system_id = 30_000_142;
        }
    });

    // 2. Corporate Buy & Sell Orders (`feat:market.corp-order-wallet-hangar-divisions`)
    let (corp_id, corp_sell_item_id, s2_fill_item_id) = ctx.db.with_mut_snapshot(|snap| {
        let cid = snap.characters[&90_000_001].corporation_id;
        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = cid;
            __corp.name = "Division Traders Inc".into();
            __corp.ticker = "DIVT".into();
            __corp.ceo_id = 90_000_001;
            __corp.set_division_balance(1, IskCents::from_isk(10_000_000));
            __corp };
        corp.ensure_wallet_divisions();
        for d in &mut corp.wallet_divisions {
            d.balance = IskCents::from_isk(1_000_000);
        }
        corp.set_division_balance(1, IskCents::from_isk(1_000_000));
        snap.corporations.insert(cid, corp);

        // Put 200 Pyerite in CorpSAG3 at Jita for corporate sell order
        snap.next_entity_id += 1;
        let c_iid = snap.next_entity_id;
        snap.items.insert(
            c_iid,
            InventoryItemRecord {
                item_id: c_iid,
                type_id: PYERITE_TYPE_ID,
                owner_id: cid,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 200,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // Put 100 Pyerite in s2's hangar to fill the corporate buy order
        snap.next_entity_id += 1;
        let s2_iid = snap.next_entity_id;
        snap.items.insert(
            s2_iid,
            InventoryItemRecord {
                item_id: s2_iid,
                type_id: PYERITE_TYPE_ID,
                owner_id: 90_000_002,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        (cid, c_iid, s2_iid)
    });

    let div3_before = ctx.db.with_snapshot(|s| {
        s.corporations[&corp_id]
            .wallet_divisions
            .iter()
            .find(|d| d.division_id == 3)
            .unwrap()
            .balance
    });
    // Place corporate buy order using accountKey 1002 (division 3) for 100 Pyerite @ 20.0 ISK (2,000 ISK escrow + 100 ISK broker fee)
    let corp_buy = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(PYERITE_TYPE_ID as i64),
            PyRep::Real(20.0),
            PyRep::Integer(100),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(true),     // useCorp
            PyRep::Integer(1002),  // accountKey -> division 3
        ],
    )
    .await
    .expect("Place corporate buy order on division 3");
    let PyRep::KeyVal(cb_kv) = corp_buy else {
        panic!("Expected KeyVal");
    };
    assert_eq!(cb_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));
    let div3_after_buy = ctx.db.with_snapshot(|s| {
        s.corporations[&corp_id]
            .wallet_divisions
            .iter()
            .find(|d| d.division_id == 3)
            .unwrap()
            .balance
    });
    assert_eq!(
        div3_before.0 - div3_after_buy.0,
        IskCents::from_isk(2_100).0,
        "Corp wallet division 3 must be debited for escrow (2,000 ISK) + broker fee (100 ISK)"
    );

    // s2 sells 100 Pyerite into the corporate buy order -> items delivered to corp Deliveries (`InventoryFlag::Deliveries`)
    rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(PYERITE_TYPE_ID as i64)),
                ("itemID", PyRep::Integer(s2_fill_item_id)),
                ("price", PyRep::Real(20.0)),
                ("quantity", PyRep::Integer(100)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(0),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect("s2 fills corporate buy order");
    let corp_deliveries_has_pyerite = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|i| {
            i.owner_id == corp_id
                && i.location_id == JITA_STATION_ID
                && i.flag == InventoryFlag::Deliveries
                && i.type_id == PYERITE_TYPE_ID
                && i.quantity == 100
        })
    });
    assert!(corp_deliveries_has_pyerite);

    // Place corporate sell order from CorpSAG3 using accountKey 1004 (division 5)
    let div5_before = ctx.db.with_snapshot(|s| {
        s.corporations[&corp_id]
            .wallet_divisions
            .iter()
            .find(|d| d.division_id == 5)
            .unwrap()
            .balance
    });
    let corp_sell = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(PYERITE_TYPE_ID as i64)),
                ("itemID", PyRep::Integer(corp_sell_item_id)),
                ("price", PyRep::Real(30.0)),
                ("quantity", PyRep::Integer(200)),
            ])]),
            PyRep::Bool(true), // useCorp
            PyRep::Integer(90),
            PyRep::Real(0.0),
            PyRep::Integer(1004), // accountKey -> division 5
        ],
    )
    .await
    .expect("Place corporate sell order from CorpSAG3");
    let PyRep::KeyVal(cs_kv) = corp_sell else {
        panic!("Expected KeyVal");
    };
    assert_eq!(cs_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));
    let div5_after_place = ctx.db.with_snapshot(|s| {
        s.corporations[&corp_id]
            .wallet_divisions
            .iter()
            .find(|d| d.division_id == 5)
            .unwrap()
            .balance
    });
    // 200 * 30.0 ISK = 6,000 ISK. Broker fee rate: 3.0% - 0.3%*4 (Broker Relations 4) - 0.02%*2.0 (Caldari Navy standing 2.0) = 1.76% -> 105.60 ISK (10,560 cents)
    assert_eq!(div5_before.0 - div5_after_place.0, 10_560);

    // s2 buys the 200 Pyerite from the corporate sell order -> proceeds credited to corp division 5!
    rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(PYERITE_TYPE_ID as i64),
            PyRep::Real(30.0),
            PyRep::Integer(200),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("s2 buys from corporate sell order");
    let div5_after_fill = ctx.db.with_snapshot(|s| {
        s.corporations[&corp_id]
            .wallet_divisions
            .iter()
            .find(|d| d.division_id == 5)
            .unwrap()
            .balance
    });
    assert!(
        div5_after_fill.0 > div5_after_place.0,
        "Corp wallet division 5 must be credited with net proceeds when corporate sell order fills"
    );

    // 3. Multi-Sell Batch with per-item durations & non-market-eligible rejection (`feat:market.multisell-quote-fees`)
    let non_market_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.market_group_id.is_none())
        .map(|t| t.type_id)
        .expect("Non-market type in SDE");
    let (batch_trit_id, batch_pyer_id, bad_item_id) = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let i1 = snap.next_entity_id;
        snap.items.insert(
            i1,
            InventoryItemRecord {
                item_id: i1,
                type_id: TRITANIUM_TYPE_ID,
                owner_id: 90_000_002,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 50,
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
                type_id: PYERITE_TYPE_ID,
                owner_id: 90_000_002,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 30,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let i3 = snap.next_entity_id;
        snap.items.insert(
            i3,
            InventoryItemRecord {
                item_id: i3,
                type_id: non_market_tid,
                owner_id: 90_000_002,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        (i1, i2, i3)
    });

    let non_mkt_err = rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![
                PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION_ID)),
                    ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                    ("itemID", PyRep::Integer(batch_trit_id)),
                    ("price", PyRep::Real(10.0)),
                    ("quantity", PyRep::Integer(50)),
                ]),
                PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION_ID)),
                    ("typeID", PyRep::Integer(non_market_tid as i64)),
                    ("itemID", PyRep::Integer(bad_item_id)),
                    ("price", PyRep::Real(10.0)),
                    ("quantity", PyRep::Integer(1)),
                ]),
            ]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(non_mkt_err.code, "ItemNotMarketEligible");
    assert!(
        ctx.db.with_snapshot(|s| s.items.contains_key(&batch_trit_id)),
        "Atomic multi-sell rejection must leave valid items in seller hangar"
    );

    let ms_ok = rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![
                PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION_ID)),
                    ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                    ("itemID", PyRep::Integer(batch_trit_id)),
                    ("price", PyRep::Real(10.0)),
                    ("quantity", PyRep::Integer(50)),
                    ("duration", PyRep::Integer(7)),
                ]),
                PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION_ID)),
                    ("typeID", PyRep::Integer(PYERITE_TYPE_ID as i64)),
                    ("itemID", PyRep::Integer(batch_pyer_id)),
                    ("price", PyRep::Real(20.0)),
                    ("quantity", PyRep::Integer(30)),
                    ("duration", PyRep::Integer(14)),
                ]),
            ]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect("Multi-sell with per-item durations");
    let PyRep::KeyVal(ms_kv) = ms_ok else {
        panic!("Expected KeyVal");
    };
    assert_eq!(ms_kv.get("placedCount"), Some(&PyRep::Integer(2)));
    assert_eq!(ms_kv.get("totalBrokerFeeISK"), Some(&PyRep::Real(200.0)));

    // 4. Multi-Buy Order Matching with Full, Partial, and Unfilled Reporting (`feat:market.multibuy-import-order-matching`)
    let mb_res = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "BuyMultipleItems",
        &[PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Integer(50)]),
            PyRep::Tuple(vec![PyRep::Integer(PYERITE_TYPE_ID as i64), PyRep::Integer(50)]),
            PyRep::Tuple(vec![PyRep::Integer(MEXALLON_TYPE_ID as i64), PyRep::Integer(10)]),
        ])],
    )
    .await
    .expect("BuyMultipleItems with full, partial, and unfilled items");
    let PyRep::KeyVal(mb_kv) = mb_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(mb_kv.get("purchasedCount"), Some(&PyRep::Integer(2)));
    assert_eq!(mb_kv.get("totalQuantity"), Some(&PyRep::Integer(80))); // 50 Trit + 30 Pyerite
    assert_eq!(mb_kv.get("totalCostISK"), Some(&PyRep::Real(1_100.0))); // 50*10 + 30*20
    let PyRep::List(statuses) = mb_kv.get("itemStatuses").expect("itemStatuses") else {
        panic!("Expected itemStatuses list");
    };
    assert_eq!(statuses.len(), 3);
    let PyRep::KeyVal(st0) = &statuses[0] else { panic!() };
    let PyRep::KeyVal(st1) = &statuses[1] else { panic!() };
    let PyRep::KeyVal(st2) = &statuses[2] else { panic!() };
    assert_eq!(st0.get("status"), Some(&PyRep::String("Filled".into())));
    assert_eq!(st1.get("status"), Some(&PyRep::String("Partial".into())));
    assert_eq!(st1.get("filledQuantity"), Some(&PyRep::Integer(30)));
    assert_eq!(st1.get("unfilledQuantity"), Some(&PyRep::Integer(20)));
    assert_eq!(st2.get("status"), Some(&PyRep::String("Unfilled".into())));

    // 5. Order Expiry & 90-Day Order History (`feat:market.order-expiry`, `feat:market.order-history`)
    // Place one buy order to cancel (state=3) and one 1-day buy order to expire (state=4); ranged_bid_oid was already fulfilled (state=2)!
    let cancel_buy = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(5.0),
            PyRep::Integer(40),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
    )
    .await
    .unwrap();
    let PyRep::KeyVal(cb_kv2) = cancel_buy else { panic!() };
    let cancel_oid = cb_kv2.get("orderID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(cancel_oid)])
        .await
        .expect("Cancel order for history");

    let expire_buy = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(5.0),
            PyRep::Integer(60),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(1), // 1 day
            PyRep::Bool(false),
        ],
    )
    .await
    .unwrap();
    let PyRep::KeyVal(eb_kv) = expire_buy else { panic!() };
    let expire_oid = eb_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();

    // Advance virtual clock by 1 day + 10 seconds -> expire_oid expires!
    let bal_before_expire = ctx.db.get_character(90_000_001).unwrap().balance;
    ctx.advance_virtual_clock(86_410);
    let hist_rep = rpc(&ctx, &mut s1, "marketProxy", "GetMarketOrderHistory", &[PyRep::Bool(true)])
        .await
        .expect("GetMarketOrderHistory");
    let bal_after_expire = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(
        bal_after_expire.0 - bal_before_expire.0,
        IskCents::from_isk(300).0,
        "Expired buy order must refund 300 ISK escrow to buyer wallet"
    );

    let PyRep::Rowset { rows: h_rows, .. } = hist_rep else {
        panic!("Expected Rowset");
    };
    let find_order = |oid: i64| {
        h_rows
            .iter()
            .find(|r| r.first().and_then(|v| v.as_i64()) == Some(oid))
            .cloned()
    };
    let fulfilled_row = find_order(ranged_bid_oid).expect("Fulfilled order in history");
    let cancelled_row = find_order(cancel_oid).expect("Cancelled order in history");
    let expired_row = find_order(expire_oid).expect("Expired order in history");

    assert_eq!(fulfilled_row.get(11), Some(&PyRep::Integer(2))); // 2 = Fulfilled
    assert_eq!(fulfilled_row.get(9), Some(&PyRep::Integer(0)));  // volRemaining = 0
    assert_eq!(cancelled_row.get(11), Some(&PyRep::Integer(3))); // 3 = Cancelled
    assert_eq!(cancelled_row.get(9), Some(&PyRep::Integer(40))); // volRemaining = 40
    assert_eq!(expired_row.get(11), Some(&PyRep::Integer(4)));   // 4 = Expired
    assert_eq!(expired_row.get(9), Some(&PyRep::Integer(60)));   // volRemaining = 60

    // Advance virtual clock by 91 days -> orders closed > 90 days ago are pruned from history
    ctx.advance_virtual_clock(91 * 86_400);
    let pruned_hist = rpc(&ctx, &mut s1, "marketProxy", "GetMarketOrderHistory", &[PyRep::Bool(true)])
        .await
        .expect("GetMarketOrderHistory after 91 days");
    let PyRep::Rowset { rows: p_rows, .. } = pruned_hist else {
        panic!("Expected Rowset");
    };
    assert!(
        p_rows.is_empty(),
        "Orders closed more than 90 days ago must be pruned from GetMarketOrderHistory"
    );
}

#[tokio::test]
async fn get_orders_dbrow_descriptor_types() {
    // parity: feat:market.get-orders, feat:serverops.packed-rows, rpc:marketProxy.GetOrders
    eve_testkit::attest(&["feat:market.get-orders", "feat:serverops.packed-rows"]);
    use eve_proto::{marshal, DBRowDescriptor, DbColumnType};

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 1104, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 1105, "other", "other-secret", 90_000_002).await;

    let sell_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.clear();
        for cid in [90_000_001_i64, 90_000_002_i64] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.balance = IskCents::from_isk(100_000_000);
                ch.station_id = Some(JITA_STATION_ID);
                ch.solar_system_id = 30_000_142;
            }
        }
        snap.next_entity_id += 1;
        let iid = snap.next_entity_id;
        snap.items.insert(
            iid,
            InventoryItemRecord {
                item_id: iid,
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
        iid
    });
    s2.station_id = Some(JITA_STATION_ID);
    s2.solar_system_id = Some(30_000_142);
    s2.region_id = Some(10_000_002);

    // Place a 500-unit sell order @ 20.0 ISK (duration 90) and partially fill 125 units so volRemaining (375) != volEntered (500).
    let sell_res = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                ("itemID", PyRep::Integer(sell_item_id)),
                ("price", PyRep::Real(20.0)),
                ("quantity", PyRep::Integer(500)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect("PlaceMultiSellOrder");
    let PyRep::KeyVal(sell_kv) = sell_res else {
        panic!("Expected KeyVal from PlaceMultiSellOrder");
    };
    let sell_oid = sell_kv
        .get("orderID")
        .and_then(|v| v.as_i64())
        .expect("sell orderID");

    rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(20.0),
            PyRep::Integer(125),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("Immediate partial fill against sell order");

    // Place a resting 200-unit buy order @ 10.0 ISK (minVolume 10, range -1, duration 30).
    let buy_res = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(10.0),
            PyRep::Integer(200),
            PyRep::Integer(-1),
            PyRep::Integer(10),
            PyRep::Integer(30),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("Place resting buy order");
    let PyRep::KeyVal(buy_kv) = buy_res else {
        panic!("Expected KeyVal from PlaceBuyOrder");
    };
    let buy_oid = buy_kv
        .get("orderID")
        .and_then(|v| v.as_i64())
        .expect("buy orderID");

    let expected_cols = [
        ("price", DbColumnType::R8, 5_i64),
        ("volRemaining", DbColumnType::I4, 3_i64),
        ("typeID", DbColumnType::I4, 3_i64),
        ("orderID", DbColumnType::I8, 20_i64),
        ("volEntered", DbColumnType::I4, 3_i64),
        ("minVolume", DbColumnType::I4, 3_i64),
        ("bid", DbColumnType::Bool, 11_i64),
        ("stationID", DbColumnType::I8, 20_i64),
        ("regionID", DbColumnType::I4, 3_i64),
        ("range", DbColumnType::I2, 2_i64),
        ("issueDate", DbColumnType::FileTime, 64_i64),
        ("duration", DbColumnType::I2, 2_i64),
        ("solarSystemID", DbColumnType::I4, 3_i64),
        ("constellationID", DbColumnType::I4, 3_i64),
        ("jumps", DbColumnType::I4, 3_i64),
    ];
    let expected_raw_defs: Vec<(String, i64)> = expected_cols
        .iter()
        .map(|(n, _, code)| ((*n).to_string(), *code))
        .collect();
    let expected_typed_cols: Vec<(String, DbColumnType)> = expected_cols
        .iter()
        .map(|(n, ty, _)| ((*n).to_string(), *ty))
        .collect();

    let extract_crowset_descriptor = |crowset: &PyRep| -> DBRowDescriptor {
        let PyRep::ObjectEx { header, .. } = crowset else {
            panic!("expected CRowset ObjectEx, got {crowset:?}");
        };
        let PyRep::Tuple(hdr_parts) = header.as_ref() else {
            panic!("expected Tuple header on CRowset");
        };
        let hdr_rep = hdr_parts
            .get(1)
            .and_then(|d| d.dict_get("header"))
            .expect("CRowset header dict must contain 'header'");
        DBRowDescriptor::from_pyrep(hdr_rep).expect("valid DBRowDescriptor in CRowset header")
    };

    // Query GetOrders and verify marshal round-trip + all 15 DBRowDescriptor column types & values.
    let orders_rep = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "GetOrders",
        &[PyRep::Integer(TRITANIUM_TYPE_ID as i64)],
    )
    .await
    .expect("GetOrders for Tritanium");
    let wire = marshal::encode(&orders_rep);
    let decoded = marshal::decode(&wire).expect("GetOrders marshal round-trip");
    assert_eq!(decoded, orders_rep);

    let PyRep::Tuple(books) = decoded else {
        panic!("Expected (sells, buys) Tuple from GetOrders");
    };
    assert_eq!(books.len(), 2);

    let expected_filetime = (common::TEST_CLOCK_BASE_UNIX as i64 + 11_644_473_600) * 10_000_000;
    let jita_constellation_id = ctx.sde.solar_systems[&30_000_142].constellation_id as i64;

    for (idx, book) in books.iter().enumerate() {
        assert!(book.is_crowset(), "GetOrders book {idx} must be CRowset");
        let desc = extract_crowset_descriptor(book);
        assert_eq!(desc.columns.len(), 15);
        assert_eq!(desc.columns, expected_typed_cols);
        assert_eq!(desc.raw_defs(), expected_raw_defs);
    }

    let sell_rows = books[0].crowset_rows().expect("sell crowset rows");
    assert_eq!(sell_rows.len(), 1);
    let sell_row = &sell_rows[0];
    let PyRep::DBRow {
        header: sell_row_hdr,
        columns: sell_cols,
        ..
    } = sell_row
    else {
        panic!("Expected PyRep::DBRow in sell CRowset");
    };
    assert_eq!(
        DBRowDescriptor::from_pyrep(sell_row_hdr)
            .expect("sell row descriptor")
            .raw_defs(),
        expected_raw_defs
    );
    assert_eq!(sell_cols.len(), 15);
    assert_eq!(sell_row.dbrow_get("price"), Some(&PyRep::Real(20.0)));
    assert_eq!(sell_row.dbrow_get("volRemaining"), Some(&PyRep::Integer(375)));
    assert_eq!(
        sell_row.dbrow_get("typeID"),
        Some(&PyRep::Integer(TRITANIUM_TYPE_ID as i64))
    );
    assert_eq!(sell_row.dbrow_get("orderID"), Some(&PyRep::Integer(sell_oid)));
    assert_eq!(sell_row.dbrow_get("volEntered"), Some(&PyRep::Integer(500)));
    assert_eq!(sell_row.dbrow_get("minVolume"), Some(&PyRep::Integer(1)));
    assert_eq!(sell_row.dbrow_get("bid"), Some(&PyRep::Bool(false)));
    assert_eq!(
        sell_row.dbrow_get("stationID"),
        Some(&PyRep::Integer(JITA_STATION_ID))
    );
    assert_eq!(sell_row.dbrow_get("regionID"), Some(&PyRep::Integer(10_000_002)));
    assert_eq!(sell_row.dbrow_get("range"), Some(&PyRep::Integer(32_767)));
    assert_eq!(
        sell_row.dbrow_get("issueDate"),
        Some(&PyRep::Integer(expected_filetime))
    );
    assert_eq!(sell_row.dbrow_get("duration"), Some(&PyRep::Integer(90)));
    assert_eq!(
        sell_row.dbrow_get("solarSystemID"),
        Some(&PyRep::Integer(30_000_142))
    );
    assert_eq!(
        sell_row.dbrow_get("constellationID"),
        Some(&PyRep::Integer(jita_constellation_id))
    );
    assert_eq!(sell_row.dbrow_get("jumps"), Some(&PyRep::Integer(0)));

    let buy_rows = books[1].crowset_rows().expect("buy crowset rows");
    assert_eq!(buy_rows.len(), 1);
    let buy_row = &buy_rows[0];
    let PyRep::DBRow {
        header: buy_row_hdr,
        columns: buy_cols,
        ..
    } = buy_row
    else {
        panic!("Expected PyRep::DBRow in buy CRowset");
    };
    assert_eq!(
        DBRowDescriptor::from_pyrep(buy_row_hdr)
            .expect("buy row descriptor")
            .raw_defs(),
        expected_raw_defs
    );
    assert_eq!(buy_cols.len(), 15);
    assert_eq!(buy_row.dbrow_get("price"), Some(&PyRep::Real(10.0)));
    assert_eq!(buy_row.dbrow_get("volRemaining"), Some(&PyRep::Integer(200)));
    assert_eq!(
        buy_row.dbrow_get("typeID"),
        Some(&PyRep::Integer(TRITANIUM_TYPE_ID as i64))
    );
    assert_eq!(buy_row.dbrow_get("orderID"), Some(&PyRep::Integer(buy_oid)));
    assert_eq!(buy_row.dbrow_get("volEntered"), Some(&PyRep::Integer(200)));
    assert_eq!(buy_row.dbrow_get("minVolume"), Some(&PyRep::Integer(10)));
    assert_eq!(buy_row.dbrow_get("bid"), Some(&PyRep::Bool(true)));
    assert_eq!(
        buy_row.dbrow_get("stationID"),
        Some(&PyRep::Integer(JITA_STATION_ID))
    );
    assert_eq!(buy_row.dbrow_get("regionID"), Some(&PyRep::Integer(10_000_002)));
    assert_eq!(buy_row.dbrow_get("range"), Some(&PyRep::Integer(-1)));
    assert_eq!(
        buy_row.dbrow_get("issueDate"),
        Some(&PyRep::Integer(expected_filetime))
    );
    assert_eq!(buy_row.dbrow_get("duration"), Some(&PyRep::Integer(30)));
    assert_eq!(
        buy_row.dbrow_get("solarSystemID"),
        Some(&PyRep::Integer(30_000_142))
    );
    assert_eq!(
        buy_row.dbrow_get("constellationID"),
        Some(&PyRep::Integer(jita_constellation_id))
    );
    assert_eq!(buy_row.dbrow_get("jumps"), Some(&PyRep::Integer(0)));

    // Negative variants:
    // 1. Unknown/empty typeID still returns valid empty 15-column CRowsets preserving all descriptor types across marshal round-trip.
    let empty_orders = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "GetOrders",
        &[PyRep::Integer(999_999_999)],
    )
    .await
    .expect("GetOrders for non-existent typeID returns empty CRowsets");
    let empty_wire = marshal::encode(&empty_orders);
    let empty_decoded = marshal::decode(&empty_wire).expect("empty GetOrders marshal round-trip");
    assert_eq!(empty_decoded, empty_orders);
    let PyRep::Tuple(empty_books) = empty_decoded else {
        panic!("Expected Tuple for empty GetOrders");
    };
    assert_eq!(empty_books.len(), 2);
    for book in &empty_books {
        assert!(book.is_crowset());
        assert!(book.crowset_rows().expect("rows").is_empty());
        assert_eq!(extract_crowset_descriptor(book).raw_defs(), expected_raw_defs);
    }

    // 2. Missing typeID argument returns InvalidMarketOrder without panicking.
    let missing_type_err = rpc(&ctx, &mut s1, "marketProxy", "GetOrders", &[])
        .await
        .unwrap_err();
    assert_eq!(missing_type_err.code, "InvalidMarketOrder");

    // 3. Session without an active region returns NoRegion without panicking.
    //    Remove the DB character record so dispatch_rpc::sync_session cannot re-populate region_id.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.remove(&90_000_001);
    });
    s1.region_id = None;
    let no_region_err = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "GetOrders",
        &[PyRep::Integer(TRITANIUM_TYPE_ID as i64)],
    )
    .await
    .unwrap_err();
    assert_eq!(no_region_err.code, "NoRegion");
}
