//! Unit tests for `market_contracts` (PLEX market, order expiry/modification, fee math, contract helpers).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::{
    InventoryFlag, IskCents, MarketOrderRecord,
};
use eve_machonet::SessionState;
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "market_orders_modify_and_auctions_tests.rs"]
mod market_orders_modify_and_auctions_tests;

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn test_ctx() -> ServerContext {
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

pub(super) fn jita_session(char_id: i64, corp_id: i64) -> SessionState {
    let mut s = SessionState::new(9101);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.station_id = Some(JITA_STATION_ID);
    s.solar_system_id = Some(30_000_142);
    s.region_id = Some(THE_FORGE_REGION_ID);
    s
}

#[test]
fn market_price_tick_remote_jumps_sales_tax_and_relist_fee_math() {
    assert!(is_valid_4_sig_fig_price(100.0));
    assert!(is_valid_4_sig_fig_price(1234.0));
    assert!(is_valid_4_sig_fig_price(12340.0));
    assert!(is_valid_4_sig_fig_price(123_400_000.0));
    assert!(is_valid_4_sig_fig_price(0.12));
    assert!(is_valid_4_sig_fig_price(99.99));
    assert!(!is_valid_4_sig_fig_price(12345.0));
    assert!(!is_valid_4_sig_fig_price(1.2345));
    assert!(!is_valid_4_sig_fig_price(100.01));
    assert!(!is_valid_4_sig_fig_price(0.0));
    assert!(!is_valid_4_sig_fig_price(-10.0));
    assert!(!is_valid_4_sig_fig_price(f64::NAN));
    assert!(!is_valid_4_sig_fig_price(f64::INFINITY));
    assert!(!is_valid_4_sig_fig_price(0.001));

    assert_eq!(max_remote_jumps_for_skill_level(0), -1);
    assert_eq!(max_remote_jumps_for_skill_level(1), 0);
    assert_eq!(max_remote_jumps_for_skill_level(2), 5);
    assert_eq!(max_remote_jumps_for_skill_level(3), 10);
    assert_eq!(max_remote_jumps_for_skill_level(4), 20);
    assert_eq!(max_remote_jumps_for_skill_level(5), 32_767);
    assert_eq!(max_remote_jumps_for_skill_level(9), 32_767);

    let gross = IskCents::from_isk(100_000_000);
    let tax0 = compute_sales_tax_cents(gross, 0);
    let tax5 = compute_sales_tax_cents(gross, 5);
    assert_eq!(tax0.0, 7_500_000_00);
    assert!(tax5.0 < tax0.0);
    assert_eq!(tax5.0, 3_375_000_00);

    // Minimum floor of 100 ISK (10_000 cents) on tiny modification
    let min_fee = compute_relist_fee_cents(
        IskCents::from_isk(100),
        IskCents::from_isk(90),
        1,
        0.03,
        5,
    );
    assert_eq!(min_fee, IskCents(10_000));

    // Price increase charges both base relist discount and 100% of incremental delta * broker_rate
    let inc_fee = compute_relist_fee_cents(
        IskCents::from_isk(10_000),
        IskCents::from_isk(20_000),
        100,
        0.02,
        0,
    );
    // new_total = 2_000_000 ISK; base = 2_000_000 * 0.02 * 0.50 = 20_000 ISK
    // inc = 1_000_000 ISK; delta = 1_000_000 * 0.02 = 20_000 ISK => total = 40_000 ISK
    assert_eq!(inc_fee, IskCents::from_isk(40_000));

    // Price decrease with Advanced Broker Relations V (discount_factor = 0.20)
    let dec_fee = compute_relist_fee_cents(
        IskCents::from_isk(20_000),
        IskCents::from_isk(10_000),
        100,
        0.02,
        5,
    );
    // new_total = 1_000_000 ISK; base = 1_000_000 * 0.02 * 0.20 = 4_000 ISK
    assert_eq!(dec_fee, IskCents::from_isk(4_000));
}

#[test]
fn sweep_expired_orders_refunds_buy_escrow_and_returns_sell_items() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let now = 1_750_000_000_u64;
    let expired_issue = now - 2 * 86_400;

    let (corp_id, initial_char_bal, initial_corp_bal, initial_plex) = ctx.db.with_mut_snapshot(|s| {
        let cid = s.characters[&char_id].corporation_id;
        s.corporations.entry(cid).or_insert_with(|| { let mut __corp = eve_db::CorporationRecord::default();
            __corp.corporation_id = cid;
            __corp.ceo_id = char_id;
            __corp.name = "SweepTestCorp".into();
            __corp.ticker = "SWTC".into();
            __corp.set_division_balance(1, IskCents::from_isk(1_000_000));
            __corp });
        let cbal = s.characters[&char_id].balance;
        let corp_bal = s.corporations[&cid].master_wallet_balance();
        let plex_before = s.plex_vault_balance(char_id);

        // 1. Expired personal buy order
        s.market_orders.insert(
            700_001,
            MarketOrderRecord {
                order_id: 700_001,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(5),
                vol_entered: 1000,
                vol_remaining: 400,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(2000),
            },
        );
        save_market_order_meta(
            s,
            700_001,
            MarketOrderMeta {
                range: -1,
                duration: 1,
                issue_unix: expired_issue,
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );

        // 2. Expired corporate buy order (division 2 -> accountKey 1001)
        s.market_orders.insert(
            700_002,
            MarketOrderRecord {
                order_id: 700_002,
                owner_id: cid,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(5),
                vol_entered: 500,
                vol_remaining: 500,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(2500),
            },
        );
        save_market_order_meta(
            s,
            700_002,
            MarketOrderMeta {
                range: -1,
                duration: 1,
                issue_unix: expired_issue,
                order_state: None,
                account_key: Some(1001),
                final_vol_remaining: None,
                closed_unix: None,
            },
        );

        // 3. Expired personal PLEX sell order
        s.market_orders.insert(
            700_003,
            MarketOrderRecord {
                order_id: 700_003,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: PLEX_TYPE_ID,
                is_bid: false,
                price: IskCents::from_isk(5_000_000),
                vol_entered: 10,
                vol_remaining: 6,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        save_market_order_meta(
            s,
            700_003,
            MarketOrderMeta {
                range: -1,
                duration: 1,
                issue_unix: expired_issue,
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );

        // 4. Expired corporate regular item sell order
        s.market_orders.insert(
            700_004,
            MarketOrderRecord {
                order_id: 700_004,
                owner_id: cid,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 35,
                is_bid: false,
                price: IskCents::from_isk(20),
                vol_entered: 300,
                vol_remaining: 150,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        save_market_order_meta(
            s,
            700_004,
            MarketOrderMeta {
                range: -1,
                duration: 1,
                issue_unix: expired_issue,
                order_state: None,
                account_key: Some(1000),
                final_vol_remaining: None,
                closed_unix: None,
            },
        );

        // 5. Active non-expired personal buy order (must remain active)
        s.market_orders.insert(
            700_005,
            MarketOrderRecord {
                order_id: 700_005,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(6),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(600),
            },
        );
        save_market_order_meta(
            s,
            700_005,
            MarketOrderMeta {
                range: -1,
                duration: 7,
                issue_unix: now - 86_400,
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );

        (cid, cbal, corp_bal, plex_before)
    });

    let swept = ctx.db.with_mut_snapshot(|s| sweep_expired_orders(s, now));
    assert_eq!(swept.len(), 4);

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.characters[&char_id].balance, IskCents(initial_char_bal.0 + 2000_00));
        assert_eq!(s.corporations[&corp_id].master_wallet_balance(), initial_corp_bal);
        assert_eq!(s.corporations[&corp_id].division_balance(2), IskCents(2500_00));
        assert_eq!(s.plex_vault_balance(char_id), initial_plex + 6);
        assert_eq!(s.market_orders[&700_001].vol_remaining, 0);
        assert_eq!(s.market_orders[&700_001].escrow, IskCents(0));
        assert_eq!(s.market_orders[&700_005].vol_remaining, 100);

        let corp_deliv = s
            .items
            .values()
            .find(|i| {
                i.owner_id == corp_id
                    && i.location_id == JITA_STATION_ID
                    && i.flag == InventoryFlag::Deliveries
                    && i.type_id == 35
            })
            .expect("expired corp sell items returned to Deliveries");
        assert_eq!(corp_deliv.quantity, 150);

        let meta1 = load_market_order_meta(s, &s.market_orders[&700_001], now);
        assert_eq!(meta1.order_state, Some(4));
        assert_eq!(meta1.final_vol_remaining, Some(400));
        assert_eq!(meta1.closed_unix, Some(now));
    });
}
