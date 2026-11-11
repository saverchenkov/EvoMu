//! Additional edge-case and branch coverage tests for `services::market_contracts`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::{
    ContractRecord, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
    WalletJournalEntry,
};
use eve_machonet::{SessionState, service_roles};
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
fn rounding_relist_clamp_and_has_expired_orders_edges() {
    assert!(round_to_4_sig_figs(f64::NAN).is_nan());
    assert_eq!(round_to_4_sig_figs(-5.0), -5.0);
    assert_eq!(round_to_4_sig_figs(0.0), 0.0);
    assert_eq!(round_to_4_sig_figs(0.001), 0.01);
    assert_eq!(round_to_4_sig_figs(9999.6), 10_000.0);
    assert!((round_to_4_sig_figs(0.015678) - 0.02).abs() < 1e-9);
    assert!((round_to_4_sig_figs(123.46) - 123.5).abs() < 1e-9);

    assert_eq!(round_to_4_sig_fig_price(0.004), 0.01);
    assert_eq!(round_to_4_sig_fig_price(f64::INFINITY), 0.01);
    assert_eq!(round_to_4_sig_fig_price(12_344.0), 12_340.0);

    // vol_remaining <= 0 hits 100 ISK (10_000 cents) floor
    assert_eq!(
        compute_relist_fee_cents(IskCents::from_isk(1000), IskCents::from_isk(1010), 0, 0.05, 5),
        IskCents(10_000)
    );
    // adv_broker_lvl > 5 is clamped to 5
    let fee_lvl5 = compute_relist_fee_cents(
        IskCents::from_isk(10_000_000),
        IskCents::from_isk(20_000_000),
        10,
        0.05,
        5,
    );
    let fee_lvl9 = compute_relist_fee_cents(
        IskCents::from_isk(10_000_000),
        IskCents::from_isk(20_000_000),
        10,
        0.05,
        9,
    );
    assert_eq!(fee_lvl5, fee_lvl9);

    let ctx = test_ctx();
    let now = 2_000_000_000_u64;
    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        // NPC order -> ignored even if expired meta
        s.market_orders.insert(
            810_001,
            MarketOrderRecord {
                order_id: 810_001,
                owner_id: 1,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(10),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: true,
                escrow: IskCents(0),
            },
        );
        save_market_order_meta(
            s,
            810_001,
            MarketOrderMeta {
                range: 32767,
                issue_unix: 100,
                duration: 1,
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
        // Zero vol_remaining -> ignored
        s.market_orders.insert(
            810_002,
            MarketOrderRecord {
                order_id: 810_002,
                owner_id: 90_000_001,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(10),
                vol_entered: 100,
                vol_remaining: 0,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        save_market_order_meta(
            s,
            810_002,
            MarketOrderMeta {
                range: 32767,
                issue_unix: 100,
                duration: 1,
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
        // Active non-expired order
        s.market_orders.insert(
            810_003,
            MarketOrderRecord {
                order_id: 810_003,
                owner_id: 90_000_001,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(10),
                vol_entered: 100,
                vol_remaining: 50,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        save_market_order_meta(
            s,
            810_003,
            MarketOrderMeta {
                range: 32767,
                issue_unix: now - 10,
                duration: 90,
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
    });
    assert!(ctx.db.with_snapshot(|s| !has_expired_orders(s, now)));
    ctx.db.with_mut_snapshot(|s| {
        save_market_order_meta(
            s,
            810_003,
            MarketOrderMeta {
                range: 32767,
                issue_unix: now - 2 * 86_400,
                duration: 1,
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
    });
    assert!(ctx.db.with_snapshot(|s| has_expired_orders(s, now)));
}

#[test]
fn market_proxy_system_asks_corp_orders_and_order_history_edges() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let mut sess = jita_session(char_id, corp_id);
    let kw = HashMap::new();

    // GetSystemAsks with missing solar_system_id returns NoSolarSystem
    let mut no_sys = sess.clone();
    no_sys.solar_system_id = None;
    let err_nosys = handle(&ctx, &mut no_sys, "marketProxy", "GetSystemAsks", &[], &kw)
        .unwrap()
        .unwrap_err();
    assert_eq!(err_nosys.code, "NoSolarSystem");

    // GetSystemAsks with explicit system_id arg
    let asks_override = handle(
        &ctx,
        &mut no_sys,
        "marketProxy",
        "GetSystemAsks",
        &[PyRep::Integer(30_000_142)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(asks_override.is_cindexed_rowset());

    // GetCorporationOrders: non-GM foreign corp AccessDenied vs GM allowed vs default caller corp
    let err_corp = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetCorporationOrders",
        &[PyRep::Integer(corp_id + 123)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_corp.code, "AccessDenied");

    let mut gm_sess = sess.clone();
    gm_sess.role = service_roles::ROLE_GML;
    let gm_corp_orders = handle(
        &ctx,
        &mut gm_sess,
        "marketProxy",
        "GetCorporationOrders",
        &[PyRep::Integer(corp_id + 123)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(gm_corp_orders.row_count().is_some());

    let own_corp_orders = handle(&ctx, &mut sess, "marketProxy", "GetCorporationOrders", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(own_corp_orders.row_count().is_some());

    // GetMarketOrderHistory: 90-day cutoff + order_state=None journal inference (expire/cancel/fulfilled)
    let now = ctx.now_unix() as u64;
    ctx.db.with_mut_snapshot(|s| {
        // Too old closed order (> 90 days ago) -> filtered out
        s.market_orders.insert(
            820_001,
            MarketOrderRecord {
                order_id: 820_001,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(10),
                vol_entered: 100,
                vol_remaining: 0,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        save_market_order_meta(
            s,
            820_001,
            MarketOrderMeta {
                range: 32767,
                duration: 90,
                issue_unix: now.saturating_sub(120 * 86_400),
                closed_unix: Some(now.saturating_sub(100 * 86_400)),
                order_state: Some(2),
                account_key: None,
                final_vol_remaining: None,
            },
        );

        // Recent closed order with order_state = None and MarketOrderExpireRefund in journal
        s.market_orders.insert(
            820_002,
            MarketOrderRecord {
                order_id: 820_002,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 35,
                is_bid: true,
                price: IskCents::from_isk(10),
                vol_entered: 50,
                vol_remaining: 0,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        save_market_order_meta(
            s,
            820_002,
            MarketOrderMeta {
                range: 32767,
                duration: 90,
                issue_unix: now.saturating_sub(3600),
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
        s.wallet_journal.push(WalletJournalEntry {
            entry_id: 820_901,
            character_id: char_id,
            entry_type: "MarketOrderExpireRefund".to_string(),
            amount: IskCents::from_isk(100),
            balance_after: IskCents::from_isk(100),
            reason: "expired order 820002".to_string(),
        });

        // Recent closed corp order with MarketOrderCancelRefund in journal
        s.market_orders.insert(
            820_003,
            MarketOrderRecord {
                order_id: 820_003,
                owner_id: corp_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 36,
                is_bid: true,
                price: IskCents::from_isk(20),
                vol_entered: 50,
                vol_remaining: 0,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        save_market_order_meta(
            s,
            820_003,
            MarketOrderMeta {
                range: 32767,
                duration: 90,
                issue_unix: now.saturating_sub(3600),
                order_state: None,
                account_key: Some(1000),
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
        s.wallet_journal.push(WalletJournalEntry {
            entry_id: 820_902,
            character_id: corp_id,
            entry_type: "MarketOrderCancelRefund".to_string(),
            amount: IskCents::from_isk(200),
            balance_after: IskCents::from_isk(200),
            reason: "cancelled order 820003".to_string(),
        });
    });

    let char_hist = handle(&ctx, &mut sess, "marketProxy", "GetMarketOrderHistory", &[], &kw)
        .unwrap()
        .unwrap();
    let PyRep::Rowset { rows, .. } = char_hist else {
        panic!("expected Rowset for GetMarketOrderHistory, got {char_hist:?}");
    };
    assert!(!rows.iter().any(|r| r.first() == Some(&PyRep::Integer(820_001))));
    assert!(rows.iter().any(|r| r.first() == Some(&PyRep::Integer(820_002))));

    let mut corp_kw = HashMap::new();
    corp_kw.insert("isCorp".to_string(), PyRep::Bool(true));
    let corp_hist = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetMarketOrderHistory",
        &[],
        &corp_kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::Rowset { rows: corp_rows, .. } = corp_hist else {
        panic!("expected Rowset for corp GetMarketOrderHistory, got {corp_hist:?}");
    };
    assert!(corp_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(820_003))));
}

#[test]
fn modify_order_validation_guards_cooldown_daytrading_and_margin_escrow() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let mut sess = jita_session(char_id, corp_id);
    let kw = HashMap::new();
    let now = ctx.now_unix() as u64;

    // 1. Missing args -> InvalidMarketOrder
    assert_eq!(
        handle(&ctx, &mut sess, "marketProxy", "ModifyCharOrder", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidMarketOrder"
    );

    // 2. Non-4-sig-fig price -> InvalidPriceTickSize
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "marketProxy",
            "ModifyCharOrder",
            &[PyRep::Integer(1), PyRep::Real(12345.0), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidPriceTickSize"
    );

    // 3. ModifyPlexCharOrder on non-PLEX order -> InvalidPlexOrder
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(1_000_000);
        // Filled order
        s.market_orders.insert(
            830_001,
            MarketOrderRecord {
                order_id: 830_001,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(100),
                vol_entered: 10,
                vol_remaining: 0,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        // Foreign order
        s.market_orders.insert(
            830_002,
            MarketOrderRecord {
                order_id: 830_002,
                owner_id: char_id + 999,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(100),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        // Order issued 10s ago -> cooldown (<300s)
        s.market_orders.insert(
            830_003,
            MarketOrderRecord {
                order_id: 830_003,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(100),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        save_market_order_meta(
            s,
            830_003,
            MarketOrderMeta {
                range: 32767,
                duration: 90,
                issue_unix: now.saturating_sub(10),
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
        // Remote order in another region (> 0 jumps) with Daytrading=0 -> DaytradingSkillRequired
        s.market_orders.insert(
            830_004,
            MarketOrderRecord {
                order_id: 830_004,
                owner_id: char_id,
                station_id: 60_008_494, // Amarr
                region_id: 10_000_043,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(100),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        save_market_order_meta(
            s,
            830_004,
            MarketOrderMeta {
                range: 32767,
                duration: 90,
                issue_unix: now.saturating_sub(600),
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
        // Clear seeded Tritanium sell orders so lowering 830_005 to 80 ISK does not cross & fill
        s.market_orders.retain(|_, o| o.type_id != 34 || (830_000..=830_010).contains(&o.order_id));
        // Margin-trading partial escrow buy order in Jita eligible for modify
        s.market_orders.insert(
            830_005,
            MarketOrderRecord {
                order_id: 830_005,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(100),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(5_000), // 50% margin ratio
            },
        );
        save_market_order_meta(
            s,
            830_005,
            MarketOrderMeta {
                range: 32767,
                duration: 90,
                issue_unix: now.saturating_sub(600),
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
        if let Some(sk) = s.skills.get_mut(&char_id) {
            sk.remove(&16595); // Daytrading
        }
    });

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "marketProxy",
            "ModifyPlexCharOrder",
            &[PyRep::Integer(830_003), PyRep::Real(110.0), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidPlexOrder"
    );

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "marketProxy",
            "ModifyCharOrder",
            &[PyRep::Integer(830_001), PyRep::Real(110.0), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OrderAlreadyFilled"
    );

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "marketProxy",
            "ModifyCharOrder",
            &[PyRep::Integer(830_002), PyRep::Real(110.0), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OrderOwnershipViolation"
    );

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "marketProxy",
            "ModifyCharOrder",
            &[PyRep::Integer(830_003), PyRep::Real(110.0), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OrderModificationCooldown"
    );

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "marketProxy",
            "ModifyCharOrder",
            &[PyRep::Integer(830_004), PyRep::Real(110.0), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "DaytradingSkillRequired"
    );

    // Lowering price on margin-trading buy order refunds proportional escrow (escrow_delta < 0)
    let mod_res = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "ModifyCharOrder",
        &[PyRep::Integer(830_005), PyRep::Real(80.0), PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(mod_res.dict_get("orderID"), Some(&PyRep::Integer(830_005)));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.market_orders[&830_005].escrow, IskCents::from_isk(4_000));
    });
}

#[test]
fn plex_history_aliases_empty_best_sell_validation_and_hangar_fallback() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let mut sess = jita_session(char_id, corp_id);
    let kw = HashMap::new();

    // Empty PLEX orders -> GetPlexBest has None prices and 0 volumes
    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.retain(|_, o| o.type_id != PLEX_TYPE_ID);
        let vault = s.plex_vault_balance(char_id);
        let _ = s.adjust_plex_vault(char_id, -vault);
        s.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(500_000_000);
        // Put 12 PLEX in station hangar to test hangar fallback when vault is empty
        s.next_entity_id += 1;
        let pid = s.next_entity_id;
        s.items.insert(
            pid,
            InventoryItemRecord {
                item_id: pid,
                type_id: PLEX_TYPE_ID,
                owner_id: char_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 12,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let empty_best = handle(&ctx, &mut sess, "marketProxy", "GetPlexBest", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(empty_best.dict_get("bestBidPrice"), Some(&PyRep::Real(0.0)));
    assert_eq!(empty_best.dict_get("bestAskPrice"), Some(&PyRep::Real(0.0)));

    // PLEX history aliases
    for m in ["GetPlexHistory", "GetPlexNewPriceHistory", "GetPlexOldPriceHistory"] {
        let r = handle(&ctx, &mut sess, "marketProxy", m, &[], &kw).unwrap().unwrap();
        assert!(r.is_crowset());
    }

    // PlacePlexSellOrder validation errors
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "marketProxy",
            "PlacePlexSellOrder",
            &[PyRep::Real(12345.0), PyRep::Integer(1), PyRep::Integer(90)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidPriceTickSize"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "marketProxy",
            "PlacePlexSellOrder",
            &[PyRep::Real(4_500_000.0), PyRep::Integer(0), PyRep::Integer(90)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "marketProxy",
            "PlacePlexSellOrder",
            &[PyRep::Real(4_500_000.0), PyRep::Integer(2), PyRep::Integer(2)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidMarketOrder"
    );

    // Hangar fallback consumes PLEX from station hangar using KeyVal arg format
    let kv = PyRep::keyval([
        ("price", PyRep::Real(4_500_000.0)),
        ("quantity", PyRep::Integer(5)),
        ("duration", PyRep::Integer(90)),
    ]);
    let ok = handle(&ctx, &mut sess, "marketProxy", "PlacePlexSellOrder", &[kv], &kw)
        .unwrap()
        .unwrap();
    assert!(ok.dict_get("orderID").and_then(PyRep::as_i64).is_some());

    // Remaining 7 PLEX in hangar -> asking 20 fails with InsufficientPlexBalance
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "marketProxy",
            "PlacePlexSellOrder",
            &[PyRep::Real(4_500_000.0), PyRep::Integer(20), PyRep::Integer(90)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientPlexBalance"
    );
}

#[test]
fn contract_proxy_page_info_finish_auction_no_winner_and_container_queries() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let mut sess = jita_session(char_id, corp_id);
    let kw = HashMap::new();

    let (auc_corp_id, wrap_id, own_cont_id, foreign_cont_id) = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let item_id = s.next_entity_id;
        s.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: 587,
                owner_id: corp_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Locked,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        s.next_entity_id += 1;
        let cid = s.next_entity_id;
        s.contracts.insert(
            cid,
            ContractRecord {
                contract_id: cid,
                issuer_id: char_id,
                issuer_corp_id: corp_id,
                for_corp: true,
                acceptor_id: None,
                contract_type: "Auction".to_string(),
                status: "Outstanding".to_string(),
                start_station_id: JITA_STATION_ID,
                end_station_id: JITA_STATION_ID,
                price: IskCents::from_isk(1_000_000),
                reward: IskCents(0),
                collateral: IskCents(0),
                item_ids: vec![item_id],
                expiration_unix: 2000,
                assignee_id: 0,
                availability: "Public".to_string(),
                buyout: IskCents(0),
                current_bid: IskCents(0),
                current_bidder_id: None,
                requested_items: Vec::new(),
                bid_history: Vec::new(),
                date_issued_unix: 1000,
                title: "Corp No-Bid Auction".to_string(),
                num_days: 1,
            },
        );

        // Courier contract wrapping a plastic wrap item
        s.next_entity_id += 1;
        let w_id = s.next_entity_id;
        s.next_entity_id += 1;
        let courier_cid = s.next_entity_id;
        s.contracts.insert(
            courier_cid,
            ContractRecord {
                contract_id: courier_cid,
                issuer_id: char_id,
                issuer_corp_id: corp_id,
                for_corp: false,
                acceptor_id: Some(char_id),
                contract_type: "Courier".to_string(),
                status: "InProgress".to_string(),
                start_station_id: JITA_STATION_ID,
                end_station_id: JITA_STATION_ID,
                price: IskCents(0),
                reward: IskCents::from_isk(50_000),
                collateral: IskCents::from_isk(100_000),
                item_ids: vec![w_id],
                expiration_unix: 2000,
                assignee_id: 0,
                availability: "Public".to_string(),
                buyout: IskCents(0),
                current_bid: IskCents(0),
                current_bidder_id: None,
                requested_items: Vec::new(),
                bid_history: Vec::new(),
                date_issued_unix: 1000,
                title: "Courier Wrap".to_string(),
                num_days: 1,
            },
        );

        // Container belonging to char vs container belonging to other_id
        s.next_entity_id += 1;
        let oc = s.next_entity_id;
        s.items.insert(
            oc,
            InventoryItemRecord {
                item_id: oc,
                type_id: 23,
                owner_id: char_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        s.next_entity_id += 1;
        let inner = s.next_entity_id;
        s.items.insert(
            inner,
            InventoryItemRecord {
                item_id: inner,
                type_id: 34,
                owner_id: char_id,
                location_id: oc,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        s.next_entity_id += 1;
        let fc = s.next_entity_id;
        s.items.insert(
            fc,
            InventoryItemRecord {
                item_id: fc,
                type_id: 23,
                owner_id: other_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        (cid, w_id, oc, fc)
    });

    // CollectMyPageInfo personal & corp
    let my_page = handle(&ctx, &mut sess, "contractProxy", "CollectMyPageInfo", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(my_page.dict_get("numOutstandingContracts").is_some());
    let corp_page = handle(
        &ctx,
        &mut sess,
        "contractProxy",
        "CollectMyPageInfo",
        &[PyRep::Bool(true)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(corp_page.dict_get("outstandingContracts").is_some());

    // NumOutstandingContracts forCorp=false / true
    let num_personal = handle(
        &ctx,
        &mut sess,
        "contractProxy",
        "NumOutstandingContracts",
        &[PyRep::Bool(false)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(num_personal.as_i64().unwrap() >= 1);

    // FinishAuction with no bids returns corp item to CorpSAG1
    let fin = handle(
        &ctx,
        &mut sess,
        "contractProxy",
        "FinishAuction",
        &[PyRep::Integer(auc_corp_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(fin, PyRep::Bool(true));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.contracts[&auc_corp_id].status, "Finished");
    });

    // FinishAuction again fails with InvalidContractState
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "contractProxy",
            "FinishAuction",
            &[PyRep::Integer(auc_corp_id)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidContractState"
    );

    // GetCourierContractFromItemID found & missing
    let found_courier = handle(
        &ctx,
        &mut sess,
        "contractProxy",
        "GetCourierContractFromItemID",
        &[PyRep::Integer(wrap_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_ne!(found_courier, PyRep::None);
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "contractProxy",
            "GetCourierContractFromItemID",
            &[PyRep::Integer(999_999_999)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );

    // GetItemsInContainer foreign -> empty List; own -> List
    let foreign_items = handle(
        &ctx,
        &mut sess,
        "contractProxy",
        "GetItemsInContainer",
        &[PyRep::Integer(foreign_cont_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(foreign_items, PyRep::List(ref l) if l.is_empty()));
    let items = handle(
        &ctx,
        &mut sess,
        "contractProxy",
        "GetItemsInContainer",
        &[PyRep::Integer(own_cont_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(items, PyRep::List(ref l) if l.len() == 1));

    // GetNumItemsInContainers with Tuple + single Integer
    let counts_tuple = handle(
        &ctx,
        &mut sess,
        "contractProxy",
        "GetNumItemsInContainers",
        &[PyRep::Tuple(vec![
            PyRep::Integer(own_cont_id),
            PyRep::Integer(foreign_cont_id),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(counts_tuple, PyRep::Dict(ref d) if d.len() == 1));

    let counts_single = handle(
        &ctx,
        &mut sess,
        "contractProxy",
        "GetNumItemsInContainers",
        &[PyRep::Integer(own_cont_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(counts_single, PyRep::Dict(ref d) if d.len() == 1));

    // Direct check for `sweep_expired_orders_and_notify` and `emit_expired_order_notifications`
    assert_eq!(sweep_expired_orders_and_notify(&ctx, 1_700_000_000), 0);
    let exp_oid = 890_991_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.insert(
            exp_oid,
            MarketOrderRecord {
                order_id: exp_oid,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(100),
                vol_entered: 5,
                vol_remaining: 5,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(500),
            },
        );
        save_market_order_meta(
            s,
            exp_oid,
            MarketOrderMeta {
                range: 32767,
                duration: 1,
                issue_unix: 1_000,
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
    });
    assert_eq!(sweep_expired_orders_and_notify(&ctx, 1_700_000_000), 1);
}
