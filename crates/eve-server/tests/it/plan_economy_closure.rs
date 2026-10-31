//! Integration tests closing canonical plan rows E-2 and E-4
//! and their blocking lifecycle and structure features.

#![forbid(unsafe_code)]

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
        let mut corp = CorporationRecord {
            corporation_id: CORP_ID,
            name: "Market Test Corp".into(),
            ticker: "MKTC".into(),
            ceo_id: P1,
            alliance_id: None,
            tax_rate: 0.05,
            master_wallet: IskCents(50_000_000_00),
            wallet_divisions: Vec::new(),
            description: String::new(),
            url: String::new(),
            hq_station_id: Some(AMARR_STATION_ID),
            members: [P1].into_iter().collect(),
            member_roles: HashMap::from([(P1, corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_TRADER)]),
            applications: HashMap::new(),
        };
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
        prof.service_settings.insert(1, 200); // 200 bps = 2.0% owner market rate (+ 0.5% SCC = 2.5% total)
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
    // parity: plan:E-4, feat:structures.service-industry, feat:lifecycle.industry-pause-and-many
    eve_testkit::attest(&[
        "plan:E-4",
        "feat:structures.service-industry",
        "feat:lifecycle.industry-pause-and-many",
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
