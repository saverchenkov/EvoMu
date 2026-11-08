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
