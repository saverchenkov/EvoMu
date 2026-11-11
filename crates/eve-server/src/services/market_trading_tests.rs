//! Unit tests for `market_trading` (regional order book, matching engine, escrow, broker fees, jump range).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::market_contracts::{JITA_STATION_ID, THE_FORGE_REGION_ID};
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord, TrainedSkillRecord,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn jita_session(char_id: i64, corp_id: i64) -> SessionState {
    let mut s = SessionState::new(9201);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.station_id = Some(JITA_STATION_ID);
    s.solar_system_id = Some(30_000_142);
    s.region_id = Some(THE_FORGE_REGION_ID);
    s
}

#[test]
fn market_order_meta_roundtrip_jump_distance_and_bid_range_matching() {
    let ctx = test_ctx();
    let now = 1_750_000_000_u64;

    // Jump distance: Jita (30000142) to itself = 0, Perimeter (30000144) = 1
    assert_eq!(solar_system_jump_distance(&ctx, 30_000_142, 30_000_142), Some(0));
    assert_eq!(solar_system_jump_distance(&ctx, 30_000_142, 30_000_144), Some(1));

    let perimeter_station = ctx
        .sde
        .solar_systems
        .get(&30_000_144)
        .and_then(|sys| sys.stations.first().map(|st| st.station_id))
        .unwrap_or(60_003_761);

    ctx.db.with_mut_snapshot(|s| {
        let mut ord = MarketOrderRecord {
            order_id: 800_001,
            owner_id: 90_000_001,
            station_id: JITA_STATION_ID,
            region_id: THE_FORGE_REGION_ID,
            type_id: 34,
            is_bid: true,
            price: IskCents::from_isk(5),
            vol_entered: 100,
            vol_remaining: 100,
            min_volume: 1,
            is_npc: false,
            escrow: IskCents::from_isk(500),
        };
        let meta = MarketOrderMeta {
            range: -1,
            duration: 90,
            issue_unix: now - 100,
            order_state: Some(1),
            account_key: Some(1002),
            final_vol_remaining: None,
            closed_unix: None,
        };
        save_market_order_meta(s, 800_001, meta);
        let loaded = load_market_order_meta(s, &ord, now);
        assert_eq!(loaded.range, -1);
        assert_eq!(loaded.duration, 90);
        assert_eq!(loaded.issue_unix, now - 100);
        assert_eq!(loaded.order_state, Some(1));
        assert_eq!(loaded.account_key, Some(1002));

        // Station-only (-1): matches Jita, rejects Perimeter
        assert!(can_bid_match_seller_station(&ctx, s, &ord, JITA_STATION_ID, now));
        assert!(!can_bid_match_seller_station(&ctx, s, &ord, perimeter_station, now));

        // 1-jump range: matches Perimeter
        save_market_order_meta(
            s,
            800_001,
            MarketOrderMeta { range: 1, ..meta },
        );
        assert!(can_bid_match_seller_station(&ctx, s, &ord, perimeter_station, now));

        // Region-wide (32767): always matches
        save_market_order_meta(
            s,
            800_001,
            MarketOrderMeta { range: 32767, ..meta },
        );
        assert!(can_bid_match_seller_station(&ctx, s, &ord, perimeter_station, now));

        // Unknown station returns None in resolve_station_solar_system
        assert_eq!(resolve_station_solar_system(&ctx, s, 999_999_999), None);
        assert_eq!(resolve_station_solar_system(&ctx, s, JITA_STATION_ID), Some(30_000_142));
        ord.station_id = 999_999_999;
        save_market_order_meta(s, 800_001, MarketOrderMeta { range: 5, ..meta });
        assert!(!can_bid_match_seller_station(&ctx, s, &ord, perimeter_station, now));
    });
}

#[test]
fn structure_market_online_and_broker_share_calculations() {
    let ctx = test_ctx();
    let struct_id = 1_040_000_777_i64;
    let mod_id = 1_040_000_778_i64;

    ctx.db.with_mut_snapshot(|s| {
        // Non-structure station defaults to online
        assert!(is_structure_market_online(s, JITA_STATION_ID));

        // Keepstar (35834) with core, fuel, ShieldVulnerable, and online Market Hub module (35892)
        s.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35834,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_144,
                name: "Perimeter Trade Keepstar".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        s.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 35_892,
                owner_id: 98_000_001,
                location_id: struct_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        assert!(is_structure_market_online(s, struct_id));
        assert_eq!(resolve_station_solar_system(&ctx, s, struct_id), Some(30_000_144));

        // Default structure broker rate = 0.015 (0.5% SCC + 1.0% owner)
        assert!((resolve_structure_broker_rate(s, struct_id) - 0.015).abs() < 1e-9);
        assert_eq!(
            structure_owner_broker_share_with_pilot(s, struct_id, IskCents::from_isk(1500), Some(100)),
            IskCents::from_isk(1000)
        );
        assert_eq!(
            structure_owner_broker_share_with_pilot(s, struct_id, IskCents(0), Some(100)),
            IskCents(0)
        );
        assert_eq!(
            structure_owner_broker_share_with_pilot(s, struct_id, IskCents::from_isk(1500), Some(0)),
            IskCents(0)
        );

        // Offline service module disables structure market
        s.module_online_states.insert(mod_id, false);
        assert!(!is_structure_market_online(s, struct_id));
        s.module_online_states.insert(mod_id, true);

        // HullReinforced or zero fuel disables structure market
        if let Some(st) = s.structures.get_mut(&struct_id) {
            st.state = "HullReinforced".to_string();
        }
        assert!(!is_structure_market_online(s, struct_id));
        if let Some(st) = s.structures.get_mut(&struct_id) {
            st.state = "ShieldVulnerable".to_string();
            st.fuel_blocks = 0;
        }
        assert!(!is_structure_market_online(s, struct_id));
    });
}

#[test]
fn market_groups_queries_orders_asks_and_escrow_summary() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let mut sess = jita_session(char_id, corp_id);
    let kw = HashMap::new();

    let groups = handle(&ctx, &mut sess, "marketProxy", "GetMarketGroups", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(groups.row_count().unwrap_or(0) > 0);

    // Seed ask & bid orders on type 34 (Tritanium) and check GetOrders, GetStationAsks, GetRegionBest, GetCharOrders, GetCharEscrow
    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.insert(
            810_001,
            MarketOrderRecord {
                order_id: 810_001,
                owner_id: 90_000_002,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(8),
                vol_entered: 500,
                vol_remaining: 500,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        s.market_orders.insert(
            810_002,
            MarketOrderRecord {
                order_id: 810_002,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(5),
                vol_entered: 200,
                vol_remaining: 200,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(750),
            },
        );
    });

    let orders = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetOrders",
        &[PyRep::Integer(34)],
        &kw,
    )
    .unwrap()
    .unwrap();
    if let PyRep::Tuple(parts) = orders {
        assert_eq!(parts.len(), 2);
        assert!(parts[0].crowset_rows().map(|r| !r.is_empty()).unwrap_or(false));
        assert!(parts[1].crowset_rows().map(|r| !r.is_empty()).unwrap_or(false));
    } else {
        panic!("expected Tuple from GetOrders");
    }

    let station_asks = handle(&ctx, &mut sess, "marketProxy", "GetStationAsks", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(station_asks.is_cindexed_rowset());

    let region_best = handle(&ctx, &mut sess, "marketProxy", "GetRegionBest", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(region_best.is_cindexed_rowset());

    let char_orders = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetCharOrders",
        &[PyRep::Integer(34)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(char_orders.row_count(), Some(1));

    let char_escrow = handle(&ctx, &mut sess, "marketProxy", "GetCharEscrow", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(char_escrow.dict_get("escrow").and_then(PyRep::as_f64), Some(750.0));
    assert_eq!(
        char_escrow.dict_get("totalRemainingCostISK").and_then(PyRep::as_f64),
        Some(1000.0)
    );
    assert_eq!(
        char_escrow.dict_get("additionalToCover").and_then(PyRep::as_f64),
        Some(250.0)
    );
}

#[test]
fn place_buy_order_resting_and_immediate_matching_execution() {
    let ctx = test_ctx();
    let buyer_id = 90_000_001_i64;
    let seller_id = 90_000_002_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&buyer_id].corporation_id);
    let mut sess = jita_session(buyer_id, corp_id);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        if let Some(ch) = s.characters.get_mut(&buyer_id) {
            ch.balance = IskCents::from_isk(100_000_000);
        }
        let mut seller = s.characters[&buyer_id].clone();
        seller.character_id = seller_id;
        seller.user_id = 2;
        seller.balance = IskCents::from_isk(1_000_000);
        s.characters.insert(seller_id, seller);

        // Give buyer Visibility II (5 jumps) and Margin Trading I (0.75 escrow)
        s.skills.entry(buyer_id).or_default().insert(
            3447,
            TrainedSkillRecord {
                type_id: 3447,
                level: 2,
                skill_points: 1415.0,
            },
        );
        s.skills.entry(buyer_id).or_default().insert(
            16597,
            TrainedSkillRecord {
                type_id: 16597,
                level: 1,
                skill_points: 250.0,
            },
        );

        // Seed resting sell order: 50 units of type 34 at 10 ISK in Jita
        s.market_orders.insert(
            820_001,
            MarketOrderRecord {
                order_id: 820_001,
                owner_id: seller_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(10),
                vol_entered: 50,
                vol_remaining: 50,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });

    // Visibility skill gate: range 10 requires Visibility III, buyer has II -> error
    let vis_err = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(34),
            PyRep::Real(12.0),
            PyRep::Integer(100),
            PyRep::Integer(10),
            PyRep::Integer(1),
            PyRep::Integer(90),
        ],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(vis_err.code, "VisibilitySkillRequired");

    // Place buy order for 120 units @ 12 ISK (matches 50 @ 10 ISK immediately, leaves 70 resting @ 12 ISK with 75% Margin Trading escrow)
    let seller_bal_before = ctx.db.with_snapshot(|s| s.characters[&seller_id].balance);
    let buy_res = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(34),
            PyRep::Real(12.0),
            PyRep::Integer(120),
            PyRep::Integer(5),
            PyRep::Integer(1),
            PyRep::Integer(90),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();

    let new_oid = buy_res.dict_get("orderID").and_then(PyRep::as_i64).unwrap();
    assert_eq!(
        buy_res.dict_get("filledImmediately").and_then(PyRep::as_bool),
        Some(false)
    );
    // 70 remaining * 12 ISK = 840 ISK * 0.75 Margin Trading I = 630 ISK escrow
    assert_eq!(
        buy_res.dict_get("escrowISK").and_then(PyRep::as_f64),
        Some(630.0)
    );

    ctx.db.with_snapshot(|s| {
        // Resting sell order 820_001 was completely filled
        assert_eq!(s.market_orders[&820_001].vol_remaining, 0);
        // Resting buy order has 70 remaining
        let resting_buy = &s.market_orders[&new_oid];
        assert_eq!(resting_buy.vol_remaining, 70);
        assert_eq!(resting_buy.escrow, IskCents::from_isk(630));
        // Seller received 50 * 10 = 500 ISK minus 7.5% sales tax (37.50 ISK) = 462.50 ISK
        assert_eq!(
            s.characters[&seller_id].balance,
            IskCents(seller_bal_before.0 + 479_00)
        );
        // Buyer received 50 units of type 34 in Jita Hangar
        let bought_stack = s
            .items
            .values()
            .find(|i| {
                i.owner_id == buyer_id
                    && i.location_id == JITA_STATION_ID
                    && i.flag == InventoryFlag::Hangar
                    && i.type_id == 34
            })
            .expect("bought items delivered to buyer hangar");
        assert!(bought_stack.quantity >= 50);
    });
}

#[test]
fn issue_601_611_612_market_tick_same_system_buy_and_wallet_item_notifications() {
    use crate::services::market_contracts::{is_valid_4_sig_fig_price, round_to_4_sig_fig_price};
    use crate::world_seeder::seed_full_market_catalog;

    // Issue #611: round_to_4_sig_fig_price and seeded orders always pass 4-sig-fig validation.
    for p in [0.5, 1.23, 99.99, 100.01, 2708.62, 9999.9, 123_456.78] {
        let rounded = round_to_4_sig_fig_price(p);
        assert!(is_valid_4_sig_fig_price(rounded), "rounded {p} -> {rounded} invalid");
    }
    assert_eq!(round_to_4_sig_fig_price(2708.62), 2709.0);

    let ctx = test_ctx();
    let seeded_count = seed_full_market_catalog(&ctx.db, &ctx.sde);
    assert!(seeded_count > 0, "expected seeded market orders");
    ctx.db.with_snapshot(|s| {
        for o in s.market_orders.values() {
            assert!(
                is_valid_4_sig_fig_price(o.price.to_wire_f64()),
                "seeded order {} (type {}) has invalid price {}",
                o.order_id,
                o.type_id,
                o.price.to_wire_f64()
            );
        }
    });

    // Issue #612 + #601: Docked at Jita II (60015260), buy player sell order at Jita IV-4 (60003760)
    // in same solar system (30000142). Verify OnAccountChange (buyer + seller) & OnItemChange emitted.
    let buyer_id = 91_200_001i64;
    let seller_id = 91_200_002i64;
    let docked_other_jita_station = 60_015_260i64;
    let seller_uid = 99_200u64;
    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        let mut seller_acc = s.accounts.values().next().cloned().unwrap();
        seller_acc.user_id = seller_uid;
        seller_acc.username = "seller_601".into();
        s.accounts.insert(seller_acc.username.clone(), seller_acc);

        let template = s.characters.values().next().cloned().unwrap();
        let mut buyer = template.clone();
        buyer.character_id = buyer_id;
        buyer.user_id = 1;
        buyer.balance = IskCents::from_isk(500_000);
        buyer.station_id = Some(docked_other_jita_station);
        s.characters.insert(buyer_id, buyer);

        let mut seller = template;
        seller.character_id = seller_id;
        seller.user_id = seller_uid;
        seller.balance = IskCents::from_isk(1_000);
        seller.station_id = Some(JITA_STATION_ID);
        s.characters.insert(seller_id, seller);

        s.market_orders.insert(
            991_001,
            MarketOrderRecord {
                order_id: 991_001,
                owner_id: seller_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_f64_isk(2709.0),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::ZERO,
            },
        );
    });
    ctx.db
        .acquire_online_lock(1, 9201, Some(buyer_id), true, ctx.now_unix())
        .expect("buyer online");
    ctx.db
        .acquire_online_lock(seller_uid, 9202, Some(seller_id), true, ctx.now_unix())
        .expect("seller online");
    let _ = ctx.drain_notifications_for_char(buyer_id);
    let _ = ctx.drain_notifications_for_char(seller_id);

    let mut buyer_sess = SessionState {
        session_id: 9201,
        user_id: Some(1),
        character_id: Some(buyer_id),
        corporation_id: Some(1_000_009),
        station_id: Some(docked_other_jita_station),
        solar_system_id: Some(30_000_142),
        region_id: Some(THE_FORGE_REGION_ID),
        ..Default::default()
    };
    let kw = HashMap::new();
    let buy_ok = handle(
        &ctx,
        &mut buyer_sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID), // Jita 4-4 while docked at Jita 60015260
            PyRep::Integer(34),
            PyRep::Real(2709.0),
            PyRep::Integer(2),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .unwrap()
    .expect("same-system Jita PlaceBuyOrder must succeed");
    assert_eq!(buy_ok.dict_get("filledImmediately"), Some(&PyRep::Bool(true)));

    let buyer_notifs = ctx.drain_notifications_for_char(buyer_id);
    let acct_notif = buyer_notifs
        .iter()
        .find(|n| n.event == "OnAccountChange" && n.args.first() == Some(&PyRep::String("cash".into())))
        .expect("expected OnAccountChange for buyer");
    assert_eq!(acct_notif.args.get(2), Some(&PyRep::Real(500_000.0 - 2.0 * 2709.0)));
    assert!(
        buyer_notifs.iter().any(|n| n.event == "OnItemChange"),
        "expected OnItemChange for buyer delivered item"
    );
    let seller_notifs = ctx.drain_notifications_for_char(seller_id);
    assert!(
        seller_notifs.iter().any(|n| n.event == "OnAccountChange" && n.args.first() == Some(&PyRep::String("cash".into()))),
        "expected OnAccountChange for seller"
    );

    // Resting order (duration=90) at remote Perimeter station (60003766) with Procurement=0 still fails with StationMismatch
    assert_eq!(
        handle(
            &ctx,
            &mut buyer_sess,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(60_003_766),
                PyRep::Integer(34),
                PyRep::Real(10.0),
                PyRep::Integer(5),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(90),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "StationMismatch"
    );
}

#[test]
fn remote_station_immediate_buy_duration_zero_succeeds_and_emits_wallet_item_notifications() {
    let ctx = test_ctx();
    let buyer_id = 91_000_051_i64;
    let seller_id = 91_000_052_i64;
    // Different station in The Forge (same region 10000002 as Jita)
    let remote_station_id = 60_003_757_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.type_id != 34);
        let base = snap.characters.values().next().unwrap().clone();
        let mut buyer = base.clone();
        buyer.character_id = buyer_id;
        buyer.user_id = 1;
        buyer.name = "RemoteBuyer".into();
        buyer.corporation_id = 1_000_009;
        buyer.balance = IskCents::from_isk(500_000);
        buyer.station_id = Some(JITA_STATION_ID);
        buyer.solar_system_id = 30_000_142;
        snap.characters.insert(buyer_id, buyer);

        let mut seller = base;
        seller.character_id = seller_id;
        seller.user_id = 2;
        seller.name = "RemoteSeller".into();
        seller.corporation_id = 1_000_009;
        seller.balance = IskCents::from_isk(100_000);
        seller.station_id = Some(remote_station_id);
        seller.solar_system_id = 30_000_142;
        snap.characters.insert(seller_id, seller);
        snap.market_orders.insert(
            820_901,
            MarketOrderRecord {
                order_id: 820_901,
                owner_id: seller_id,
                station_id: remote_station_id,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(100),
                vol_entered: 20,
                vol_remaining: 20,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });

    let mut buyer_sess = jita_session(buyer_id, 1_000_009);
    ctx.db.acquire_online_lock(1, 9201, Some(buyer_id), true, ctx.now_unix()).unwrap();
    ctx.db.acquire_online_lock(2, 9202, Some(seller_id), true, ctx.now_unix()).unwrap();
    // Remote non-immediate limit buy order (duration=90) without Procurement still rejects with StationMismatch
    let limit_err = handle(
        &ctx,
        &mut buyer_sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(remote_station_id),
            PyRep::Integer(34),
            PyRep::Real(100.0),
            PyRep::Integer(5),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
            PyRep::None,
        ],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(limit_err.code, "StationMismatch");

    let _ = ctx.drain_notifications_for_char(buyer_id);
    let _ = ctx.drain_notifications_for_char(seller_id);

    // Remote immediate buy order (duration=0) matches resting remote ask and delivers to remote station hangar (#659)
    let res = handle(
        &ctx,
        &mut buyer_sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(remote_station_id),
            PyRep::Integer(34),
            PyRep::Real(100.0),
            PyRep::Integer(5),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Bool(false),
            PyRep::None,
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        res.dict_get("filledImmediately").and_then(PyRep::as_bool),
        Some(true)
    );

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.market_orders[&820_901].vol_remaining, 15);
        assert!(snap.items.values().any(|i| {
            i.owner_id == buyer_id
                && i.location_id == remote_station_id
                && i.flag == InventoryFlag::Hangar
                && i.type_id == 34
                && i.quantity == 5
        }));
    });

    let buyer_notifs = ctx.drain_notifications_for_char(buyer_id);
    assert!(
        buyer_notifs.iter().any(|n| n.event == "OnAccountChange"),
        "expected OnAccountChange for immediate buyer, got {:?}",
        buyer_notifs.iter().map(|n| &n.event).collect::<Vec<_>>()
    );
    assert!(
        buyer_notifs.iter().any(|n| n.event == "OnItemChange"),
        "expected OnItemChange for immediate buyer delivery"
    );
    let seller_notifs = ctx.drain_notifications_for_char(seller_id);
    assert!(
        seller_notifs.iter().any(|n| n.event == "OnAccountChange"),
        "expected OnAccountChange for matched seller"
    );
}

#[test]
fn issue_631_resting_buy_order_deducts_wallet_and_emits_account_change() {
    let ctx = test_ctx();
    let buyer_id = 91_200_631i64;
    let start_isk = 500_000_000_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        let mut buyer = s.characters.values().next().cloned().unwrap();
        buyer.character_id = buyer_id;
        buyer.user_id = 1;
        buyer.balance = IskCents::from_isk(start_isk);
        buyer.station_id = Some(JITA_STATION_ID);
        s.characters.insert(buyer_id, buyer);
    });
    ctx.db
        .acquire_online_lock(1, 9631, Some(buyer_id), true, ctx.now_unix())
        .expect("buyer online");
    let _ = ctx.drain_notifications_for_char(buyer_id);

    let mut buyer_sess = jita_session(buyer_id, 1_000_009);
    let kw = HashMap::new();

    // Place resting buy order matching #631: 1x type 34 @ 1.00 ISK, duration 90, expectedBrokersFee=100.0
    let res = handle(
        &ctx,
        &mut buyer_sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(34),
            PyRep::Real(1.0),
            PyRep::Integer(1),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
            PyRep::Real(100.0),
        ],
        &kw,
    )
    .unwrap()
    .expect("PlaceBuyOrder duration=90 succeeds");
    assert_eq!(res.dict_get("filledImmediately").and_then(PyRep::as_bool), Some(false));
    let broker_fee = res.dict_get("brokerFeeISK").and_then(PyRep::as_f64).unwrap();
    let escrow_isk = res.dict_get("escrowISK").and_then(PyRep::as_f64).unwrap();
    let expected_bal = (start_isk as f64) - broker_fee - escrow_isk;
    assert_eq!(broker_fee, 100.0);
    assert_eq!(escrow_isk, 1.0);

    let notifs = ctx.drain_notifications_for_char(buyer_id);
    let acct_notif = notifs
        .iter()
        .find(|n| n.event == "OnAccountChange" && n.args.first() == Some(&PyRep::String("cash".into())))
        .expect("resting PlaceBuyOrder must emit OnAccountChange for buyer wallet");
    assert_eq!(acct_notif.args.get(1), Some(&PyRep::Integer(buyer_id)));
    assert_eq!(acct_notif.args.get(2), Some(&PyRep::Real(expected_bal)));
    assert!(notifs.iter().any(|n| n.event == "OnOwnOrdersChanged"));

    let live_bal = crate::services::market_contracts::handle(
        &ctx,
        &mut buyer_sess,
        "account",
        "GetCashBalance",
        &[],
        &kw,
    )
    .unwrap()
    .expect("account.GetCashBalance ok")
    .as_f64()
    .unwrap();
    assert_eq!(live_bal, expected_bal);
}

#[test]
fn market_v24_resting_buy_order_visibility_and_account_item_notifications() {
    let ctx = test_ctx();
    let buyer_id = 91_000_061_i64;
    let seller_id = 91_000_062_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.type_id != 34);
        let base = snap.characters.values().next().unwrap().clone();
        let mut buyer = base.clone();
        buyer.character_id = buyer_id;
        buyer.user_id = 1;
        buyer.name = "V24MarketBuyer".into();
        buyer.corporation_id = 1_000_009;
        buyer.balance = IskCents::from_isk(1_000_000);
        buyer.station_id = Some(JITA_STATION_ID);
        buyer.solar_system_id = 30_000_142;
        snap.characters.insert(buyer_id, buyer);

        let mut seller = base;
        seller.character_id = seller_id;
        seller.user_id = 2;
        seller.name = "V24MarketSeller".into();
        seller.corporation_id = 1_000_009;
        seller.balance = IskCents::from_isk(1_000_000);
        seller.station_id = Some(JITA_STATION_ID);
        seller.solar_system_id = 30_000_142;
        snap.characters.insert(seller_id, seller);
        snap.next_entity_id += 1;
        let sell_stack_id = snap.next_entity_id;
        snap.items.insert(
            sell_stack_id,
            InventoryItemRecord {
                item_id: sell_stack_id,
                type_id: 34,
                owner_id: seller_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut buyer_sess = jita_session(buyer_id, 1_000_009);
    ctx.db.acquire_online_lock(1, 9201, Some(buyer_id), true, ctx.now_unix()).unwrap();
    ctx.db.acquire_online_lock(2, 9202, Some(seller_id), true, ctx.now_unix()).unwrap();
    let mut seller_sess = jita_session(seller_id, 1_000_009);
    let _ = ctx.drain_notifications_for_char(buyer_id);
    let _ = ctx.drain_notifications_for_char(seller_id);

    // 1. Place V24 9-arg resting buy order with duration=1 & broker percentage 0.03
    let place_buy = handle(
        &ctx,
        &mut buyer_sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(34),
            PyRep::Real(7950.0),
            PyRep::Integer(1),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Bool(false),
            PyRep::Real(0.03),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    let buy_oid = place_buy.dict_get("orderID").and_then(PyRep::as_i64).unwrap();
    assert_eq!(
        place_buy.dict_get("filledImmediately").and_then(PyRep::as_bool),
        Some(false)
    );
    let buy_notifs = ctx.drain_notifications_for_char(buyer_id);
    assert!(buy_notifs.iter().any(|n| n.event == "OnAccountChange"));
    assert!(buy_notifs.iter().any(|n| n.event == "OnOwnOrdersChanged"));

    // Verify order appears in GetOrders(34) Buyers rowset AND GetCharOrders()
    let get_orders = handle(
        &ctx,
        &mut buyer_sess,
        "marketProxy",
        "GetOrders",
        &[PyRep::Integer(34)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::Tuple(parts) = get_orders else {
        panic!("expected 2-tuple from GetOrders");
    };
    let buyers_rows = parts[1].crowset_rows().expect("Buyers crowset rows");
    assert!(
        buyers_rows
            .iter()
            .any(|r| r.dbrow_get("orderID").and_then(PyRep::as_i64) == Some(buy_oid)),
        "Buyers rowset should include order {buy_oid}"
    );
    let char_orders = handle(&ctx, &mut buyer_sess, "marketProxy", "GetCharOrders", &[], &kw)
        .unwrap()
        .unwrap();
    let char_has_oid = char_orders
        .crowset_rows()
        .map(|rows| {
            rows.iter()
                .any(|r| r.dbrow_get("orderID").and_then(PyRep::as_i64) == Some(buy_oid))
        })
        .unwrap_or_else(|| format!("{char_orders:?}").contains(&buy_oid.to_string()));
    assert!(char_has_oid, "GetCharOrders should include order {buy_oid}");

    // 2. ModifyCharOrder price upward emits OnAccountChange (#660)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(o) = snap.market_orders.get(&buy_oid).cloned() {
            let mut m = load_market_order_meta(snap, &o, ctx.now_unix());
            m.issue_unix = ctx.now_unix().saturating_sub(400);
            save_market_order_meta(snap, buy_oid, m);
        }
    });
    let mod_res = crate::services::market_contracts::handle(
        &ctx,
        &mut buyer_sess,
        "marketProxy",
        "ModifyCharOrder",
        &[PyRep::Integer(buy_oid), PyRep::Real(8000.0)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(mod_res.dict_get("newPrice").and_then(PyRep::as_f64), Some(8000.0));
    assert!(ctx
        .drain_notifications_for_char(buyer_id)
        .iter()
        .any(|n| n.event == "OnAccountChange"));

    // 3. PlaceMultiSellOrder partial/resting sell emits OnItemChange & OnAccountChange (#660)
    let sell_res = handle(
        &ctx,
        &mut seller_sess,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(34)),
                ("itemID", PyRep::Integer(0)),
                ("price", PyRep::Real(9000.0)),
                ("quantity", PyRep::Integer(10)),
                ("duration", PyRep::Integer(90)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    let sell_oid = sell_res.dict_get("orderID").and_then(PyRep::as_i64).unwrap();
    let sell_notifs = ctx.drain_notifications_for_char(seller_id);
    assert!(sell_notifs.iter().any(|n| n.event == "OnItemChange"));
    assert!(sell_notifs.iter().any(|n| n.event == "OnAccountChange"));

    // 4. BuyMultipleItems emits OnAccountChange (buyer & seller) and OnItemChange (buyer)
    let _ = ctx.drain_notifications_for_char(buyer_id);
    let _ = ctx.drain_notifications_for_char(seller_id);
    let multi_buy = handle(
        &ctx,
        &mut buyer_sess,
        "marketProxy",
        "BuyMultipleItems",
        &[
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(34),
                PyRep::Integer(2),
                PyRep::Real(9000.0),
            ])]),
            PyRep::Integer(JITA_STATION_ID),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        multi_buy.dict_get("totalQuantity").and_then(PyRep::as_i64),
        Some(2)
    );
    let mb_buyer_notifs = ctx.drain_notifications_for_char(buyer_id);
    assert!(mb_buyer_notifs.iter().any(|n| n.event == "OnAccountChange"));
    assert!(mb_buyer_notifs.iter().any(|n| n.event == "OnItemChange"));
    assert!(ctx
        .drain_notifications_for_char(seller_id)
        .iter()
        .any(|n| n.event == "OnAccountChange"));

    // 5. CancelCharOrder on buy order emits OnAccountChange, on sell order emits OnItemChange (#660)
    handle(
        &ctx,
        &mut buyer_sess,
        "marketProxy",
        "CancelCharOrder",
        &[PyRep::Integer(buy_oid), PyRep::Integer(JITA_STATION_ID)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(ctx
        .drain_notifications_for_char(buyer_id)
        .iter()
        .any(|n| n.event == "OnAccountChange"));

    handle(
        &ctx,
        &mut seller_sess,
        "marketProxy",
        "CancelCharOrder",
        &[PyRep::Integer(sell_oid), PyRep::Integer(JITA_STATION_ID)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(ctx
        .drain_notifications_for_char(seller_id)
        .iter()
        .any(|n| n.event == "OnItemChange"));
}

#[test]
fn v24_immediate_place_buy_order_and_cancel_buy_order_emit_wallet_and_item_notifications_700_714() {
    let ctx = test_ctx();
    let buyer_id = 90_991_001_i64;
    let seller_id = 90_991_002_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.type_id != 1230);
        let base = snap.characters.values().next().unwrap().clone();
        let mut buyer = base.clone();
        buyer.character_id = buyer_id;
        buyer.user_id = 1;
        buyer.name = "V24Buyer700".into();
        buyer.corporation_id = 1_000_009;
        buyer.balance = IskCents::from_isk(100_000);
        buyer.station_id = Some(JITA_STATION_ID);
        buyer.solar_system_id = 30_000_142;
        snap.characters.insert(buyer_id, buyer);

        let mut seller = base;
        seller.character_id = seller_id;
        seller.user_id = 2;
        seller.name = "V24Seller700".into();
        seller.corporation_id = 1_000_009;
        seller.balance = IskCents::from_isk(10_000);
        seller.station_id = Some(JITA_STATION_ID);
        seller.solar_system_id = 30_000_142;
        snap.characters.insert(seller_id, seller);

        snap.next_entity_id += 1;
        let sell_oid = snap.next_entity_id;
        snap.market_orders.insert(
            sell_oid,
            eve_db::MarketOrderRecord {
                order_id: sell_oid,
                type_id: 1230, // Veldspar
                owner_id: seller_id,
                station_id: JITA_STATION_ID,
                region_id: 10_000_002,
                price: IskCents::from_isk(2000),
                vol_entered: 10,
                vol_remaining: 10,
                is_bid: false,
                is_npc: false,
                min_volume: 1,
                escrow: IskCents(0),
            },
        );
    });

    let mut buyer_sess = jita_session(buyer_id, 1_000_009);
    ctx.db.acquire_online_lock(1, 9701, Some(buyer_id), true, ctx.now_unix()).unwrap();
    let _ = ctx.drain_notifications_for_char(buyer_id);

    // #700: Immediate PlaceBuyOrder(60003760, 1230, 2120.0, 1, -1, 1, 0, False, None)
    let buy_res = handle(
        &ctx,
        &mut buyer_sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(1230),
            PyRep::Real(2120.0),
            PyRep::Integer(1),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Bool(false),
            PyRep::None,
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        buy_res.dict_get("filledImmediately").and_then(PyRep::as_bool),
        Some(true)
    );
    let bal_after_buy = ctx.db.get_character(buyer_id).unwrap().balance;
    assert!(
        bal_after_buy < IskCents::from_isk(100_000),
        "immediate fill must deduct ISK: {bal_after_buy:?}"
    );
    let has_veldspar = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|it| {
            it.owner_id == buyer_id
                && it.type_id == 1230
                && it.location_id == JITA_STATION_ID
                && it.quantity >= 1
        })
    });
    assert!(has_veldspar, "immediate buy must deliver Veldspar to buyer hangar");
    let buy_notifs = ctx.drain_notifications_for_char(buyer_id);
    assert!(
        buy_notifs.iter().any(|n| n.event == "OnAccountChange"),
        "immediate PlaceBuyOrder must emit OnAccountChange (#700)"
    );
    assert!(
        buy_notifs.iter().any(|n| n.event == "OnItemChange"),
        "immediate PlaceBuyOrder must emit OnItemChange (#700)"
    );

    // #714: Resting 1.00 ISK buy order -> CancelCharOrder refunds 1.00 ISK & emits OnAccountChange
    let bal_before_escrow = ctx.db.get_character(buyer_id).unwrap().balance;
    let resting_buy = handle(
        &ctx,
        &mut buyer_sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(1230),
            PyRep::Real(1.0),
            PyRep::Integer(1),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Bool(false),
            PyRep::None,
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    let order_id = resting_buy.dict_get("orderID").and_then(PyRep::as_i64).unwrap();
    let bal_with_escrow = ctx.db.get_character(buyer_id).unwrap().balance;
    assert!(bal_with_escrow <= IskCents(bal_before_escrow.0 - 100));
    let _ = ctx.drain_notifications_for_char(buyer_id);

    handle(
        &ctx,
        &mut buyer_sess,
        "marketProxy",
        "CancelCharOrder",
        &[PyRep::Integer(order_id), PyRep::Integer(JITA_STATION_ID)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let bal_after_cancel = ctx.db.get_character(buyer_id).unwrap().balance;
    assert_eq!(
        bal_after_cancel,
        IskCents(bal_with_escrow.0 + 100),
        "CancelCharOrder must refund 1.00 ISK escrow (#714)"
    );
    let cancel_notifs = ctx.drain_notifications_for_char(buyer_id);
    assert!(
        cancel_notifs.iter().any(|n| n.event == "OnAccountChange"),
        "CancelCharOrder must emit OnAccountChange (#714)"
    );
    assert!(
        cancel_notifs.iter().any(|n| n.event == "OnOwnOrdersChanged"),
        "CancelCharOrder must emit OnOwnOrdersChanged (#714)"
    );
}

#[test]
fn place_buy_order_v24_mining_laser_immediate_crosses_seeded_sell_order_issue_731() {
    let ctx = test_ctx();
    let buyer_id = 90_000_001;
    let seller_id = 90_000_002;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(buyer) = snap.characters.get_mut(&buyer_id) {
            buyer.balance = IskCents::from_isk(1_000_000);
        }
        if !snap.characters.contains_key(&seller_id) {
            let mut seller = snap.characters[&buyer_id].clone();
            seller.character_id = seller_id;
            seller.name = "MiningLaserSeller".into();
            seller.station_id = Some(JITA_STATION_ID);
            seller.solar_system_id = 30_000_142;
            snap.characters.insert(seller_id, seller);
        }
        snap.market_orders.insert(
            99_001_731,
            eve_db::MarketOrderRecord {
                order_id: 99_001_731,
                type_id: 483, // Mining Laser I
                owner_id: seller_id,
                station_id: JITA_STATION_ID,
                region_id: 10_000_002,
                price: IskCents::from_f64_isk(12_566.15),
                vol_entered: 10,
                vol_remaining: 10,
                is_bid: false,
                is_npc: false,
                min_volume: 1,
                escrow: IskCents(0),
            },
        );
    });

    let mut buyer_sess = jita_session(buyer_id, 1_000_009);
    ctx.db.acquire_online_lock(1, 9731, Some(buyer_id), true, ctx.now_unix()).unwrap();
    let _ = ctx.drain_notifications_for_char(buyer_id);

    // Live V24.01 payload: PlaceBuyOrder(60003760, 483, 12570.0, 1, -1, 1, 1, False, 0.03)
    let buy_res = handle(
        &ctx,
        &mut buyer_sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(483),
            PyRep::Real(12_570.0),
            PyRep::Integer(1),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Bool(false),
            PyRep::Real(0.03),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();

    assert_eq!(
        buy_res.dict_get("filledImmediately").and_then(PyRep::as_bool),
        Some(true),
        "Immediate V24.01 PlaceBuyOrder must cross resting Mining Laser I sell order (#731)"
    );
    let bal_after = ctx.db.get_character(buyer_id).unwrap().balance;
    assert!(
        bal_after < IskCents::from_isk(1_000_000),
        "Immediate buy must deduct ISK from buyer wallet: {bal_after:?}"
    );
    let has_laser = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|it| {
            it.owner_id == buyer_id
                && it.type_id == 483
                && it.location_id == JITA_STATION_ID
                && it.quantity >= 1
        })
    });
    assert!(has_laser, "Immediate buy must deliver Mining Laser I to buyer hangar");

    let notifs = ctx.drain_notifications_for_char(buyer_id);
    assert!(notifs.iter().any(|n| n.event == "OnAccountChange"));
    assert!(notifs.iter().any(|n| n.event == "OnItemChange"));
}

#[test]
fn test_issue_789_795_place_buy_order_v24_buystuff_9tuple_and_self_trade_account_change_order() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.clear();
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(1_000_000);
        }
        // Self-owned sell order for type 3651 at 165.0 ISK in Jita
        snap.market_orders.insert(
            99_001_795,
            eve_db::MarketOrderRecord {
                order_id: 99_001_795,
                type_id: 3651,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: 10_000_002,
                price: IskCents::from_f64_isk(165.0),
                vol_entered: 10,
                vol_remaining: 10,
                is_bid: false,
                is_npc: false,
                min_volume: 1,
                escrow: IskCents(0),
            },
        );
    });

    let mut sess = jita_session(char_id, 1_000_009);
    ctx.db
        .acquire_online_lock(1, 9795, Some(char_id), true, ctx.now_unix())
        .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);

    // Live V24.01 marketsvc.py:298 BuyStuff 9-tuple:
    // (stationID, typeID, price, quantity, orderRange=32767, minVolume=1, orderID=None, useCorp=0, duration=0)
    let buy_res = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(3651),
            PyRep::Real(165.0),
            PyRep::Integer(2),
            PyRep::Integer(32767),
            PyRep::Integer(1),
            PyRep::None,
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .unwrap()
    .expect("V24.01 BuyStuff 9-tuple with args[6]=None and useCorp=Integer(0) must succeed");

    assert_eq!(
        buy_res.dict_get("filledImmediately").and_then(PyRep::as_bool),
        Some(true)
    );
    let final_bal = ctx.db.get_character(char_id).unwrap().balance.to_wire_f64();
    let acct_balances: Vec<f64> = ctx
        .drain_notifications_for_char(char_id)
        .into_iter()
        .filter(|n| n.event == "OnAccountChange")
        .filter_map(|n| n.args.get(2).and_then(PyRep::as_f64))
        .collect();
    assert!(
        acct_balances.len() >= 2,
        "Self-trade must emit buyer debit and seller credit OnAccountChange notifications"
    );
    assert!(
        acct_balances[0] < acct_balances[1],
        "Buyer debit OnAccountChange must precede seller credit so last notification is final balance: {acct_balances:?}"
    );
    assert!(
        (acct_balances.last().copied().unwrap() - final_bal).abs() < 1e-2,
        "Last OnAccountChange balance must equal final wallet balance ({final_bal})"
    );
}

#[test]
fn place_multi_sell_order_full_stack_emits_ixflag_and_ixsingleton_in_tombstone() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let mut sess = jita_session(char_id, corp_id);
    let kw = HashMap::new();
    let stack_id = 889_901_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            stack_id,
            InventoryItemRecord {
                item_id: stack_id,
                type_id: 34,
                owner_id: char_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let uid = ctx.db.with_mut_snapshot(|s| {
        let uid = s.characters[&char_id].user_id;
        if !s.accounts.values().any(|a| a.user_id == uid) {
            s.accounts.insert(
                format!("mkt_char_{uid}"),
                eve_db::AccountRecord {
                    user_id: uid,
                    username: format!("mkt_char_{uid}"),
                    password_token: String::new(),
                    role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
        uid
    });
    ctx.db
        .acquire_online_lock(uid, 1, Some(char_id), true, ctx.now_unix())
        .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);

    let entry = PyRep::keyval([
        ("itemID", PyRep::Integer(stack_id)),
        ("typeID", PyRep::Integer(34)),
        ("price", PyRep::Real(12.5)),
        ("quantity", PyRep::Integer(10)),
        ("officeID", PyRep::None),
    ]);
    handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![entry]),
            PyRep::Integer(0),
            PyRep::Integer(90),
            PyRep::None,
        ],
        &kw,
    )
    .unwrap()
    .expect("PlaceMultiSellOrder full stack");

    let item_notifs: Vec<_> = ctx
        .drain_notifications_for_char(char_id)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .collect();
    assert_eq!(item_notifs.len(), 1, "Expected 1 OnItemChange tombstone notification");
    let PyRep::Dict(change_entries) = &item_notifs[0].args[1] else {
        panic!("expected Dict for OnItemChange change map");
    };
    let old_loc = change_entries
        .iter()
        .find(|(k, _)| k.as_i64() == Some(3))
        .and_then(|(_, v)| v.as_i64());
    let old_flag = change_entries
        .iter()
        .find(|(k, _)| k.as_i64() == Some(4))
        .and_then(|(_, v)| v.as_i64());
    let old_qty = change_entries
        .iter()
        .find(|(k, _)| k.as_i64() == Some(9))
        .and_then(|(_, v)| v.as_i64());
    assert_eq!(old_loc, Some(JITA_STATION_ID));
    assert_eq!(old_flag, Some(InventoryFlag::Hangar as i64));
    assert_eq!(old_qty, Some(10));
    assert!(
        !change_entries.iter().any(|(k, _)| k.as_i64() == Some(10)),
        "Non-singleton stack must not include ixSingleton (10) in tombstone"
    );

    // Verify assembled/singleton items are rejected by PlaceMultiSellOrder with CannotSellAssembledItem
    let singleton_id = 889_902_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            singleton_id,
            InventoryItemRecord {
                item_id: singleton_id,
                type_id: 34,
                owner_id: char_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    let singleton_entry = PyRep::keyval([
        ("itemID", PyRep::Integer(singleton_id)),
        ("typeID", PyRep::Integer(34)),
        ("price", PyRep::Real(12.5)),
        ("quantity", PyRep::Integer(1)),
        ("officeID", PyRep::None),
    ]);
    let err = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![singleton_entry]),
            PyRep::Integer(0),
            PyRep::Integer(90),
            PyRep::None,
        ],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "CannotSellAssembledItem");
    assert!(ctx
        .drain_notifications_for_char(char_id)
        .into_iter()
        .all(|n| n.event != "OnItemChange"));
    ctx.db.with_snapshot(|s| assert!(s.items.contains_key(&singleton_id)));
}

#[test]
fn place_buy_order_immediate_non_buy_stuff_enforces_visibility_skill() {
    let ctx = test_ctx();
    let buyer_id = 90_000_001_i64;
    let seller_id = 90_000_002_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        if let Some(ch) = s.characters.get_mut(&buyer_id) {
            ch.balance = IskCents::from_isk(100_000_000);
        }
        let mut seller = s.characters[&buyer_id].clone();
        seller.character_id = seller_id;
        seller.user_id = 2;
        seller.balance = IskCents::from_isk(1_000_000);
        s.characters.insert(seller_id, seller);
        // Ensure buyer has Visibility = 0 (max_remote_jumps = 0)
        s.skills.entry(buyer_id).or_default().remove(&3447);
        s.market_orders.insert(
            820_501,
            MarketOrderRecord {
                order_id: 820_501,
                owner_id: seller_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(10),
                vol_entered: 20,
                vol_remaining: 20,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });

    let corp_id = ctx.db.with_snapshot(|s| s.characters[&buyer_id].corporation_id);
    let mut sess = jita_session(buyer_id, corp_id);

    // 1. Non-BuyStuff 8-arg immediate order (args[6] = Integer(0), duration = 0) with remote range 32767
    //    must still enforce VisibilitySkillRequired.
    let err = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(34),
            PyRep::Real(10.0),
            PyRep::Integer(2),
            PyRep::Integer(32767),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Bool(false),
        ],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "VisibilitySkillRequired");

    // 2. BuyStuff 9-arg signature with duration = 1 (> 0) and range = 32767 also enforces VisibilitySkillRequired.
    let err_bs_dur = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(34),
            PyRep::Real(10.0),
            PyRep::Integer(2),
            PyRep::Integer(32767),
            PyRep::Integer(1),
            PyRep::None,
            PyRep::Integer(0),
            PyRep::Integer(1),
        ],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_bs_dur.code, "VisibilitySkillRequired");

    // 3. BuyStuff 9-arg signature with duration = 0 exempts VisibilitySkillRequired even with Visibility = 0.
    let ok_bs = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(34),
            PyRep::Real(10.0),
            PyRep::Integer(2),
            PyRep::Integer(32767),
            PyRep::Integer(1),
            PyRep::None,
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .unwrap()
    .expect("BuyStuff duration=0 bypasses Visibility check");
    assert_eq!(
        ok_bs.dict_get("filledImmediately").and_then(PyRep::as_bool),
        Some(true)
    );

    // 4. Non-BuyStuff immediate order at station range (-1 <= 0) passes Visibility check cleanly.
    let ok_local = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(34),
            PyRep::Real(10.0),
            PyRep::Integer(2),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Bool(false),
        ],
        &kw,
    )
    .unwrap()
    .expect("Non-BuyStuff duration=0 at station range succeeds");
    assert_eq!(
        ok_local.dict_get("filledImmediately").and_then(PyRep::as_bool),
        Some(true)
    );
}

#[cfg(test)]
#[path = "market_trading_extra_unit_tests.rs"]
mod market_trading_extra_unit_tests;
