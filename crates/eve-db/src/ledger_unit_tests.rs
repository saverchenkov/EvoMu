//! Targeted unit and mutation-killing tests for `ledger.rs`.

use super::*;
use crate::types::{CharacterRecord, CorporationRecord, MarketOrderRecord, NeuralAttributes};
use std::collections::HashMap;

fn make_char(char_id: i64, user_id: u64, plex_balance: i64) -> CharacterRecord {
    CharacterRecord {
        character_id: char_id,
        user_id,
        name: format!("Char {char_id}"),
        corporation_id: 98_001,
        alliance_id: None,
        war_faction_id: None,
        station_id: Some(60_003_760),
        structure_id: None,
        solar_system_id: 30_000_142,
        constellation_id: 20_000_020,
        region_id: 10_000_002,
        active_ship_id: 200_001,
        balance: IskCents::from_isk(10_000),
        plex_balance,
        loyalty_points: HashMap::new(),
        standings: HashMap::new(),
        security_status: 0.0,
        clone_grade: 1,
        home_station_id: 60_003_760,
        race_id: 1,
        bloodline_id: 1,
        ancestry_id: 1,
        school_id: 17,
        neural: NeuralAttributes::default(),
        skill_points: 400_000.0,
        activity_status: None,
        free_skill_points: 0.0,
        last_clone_jump_unix: 0,
        jump_fatigue_minutes: 0.0,
        jump_cooldown_until_unix: 0,
        jump_fatigue_until_unix: 0,
        description: String::new(),
        title: String::new(),
        gender: 1,
        create_date_unix: 1_700_000_000,
        fleet_id: None,
        autopilot_waypoints: Vec::new(),
        version: 1,
        system_epoch: 1,
    }
}

#[test]
fn journal_meta_corporate_divisions_and_market_fill_accounting() {
    let mut snap = WorldStateSnapshot::default();
    snap.characters.insert(90_001, make_char(90_001, 1, 0));
    snap.characters.insert(90_002, make_char(90_002, 2, 0));
    snap.corporations.insert(
        98_001,
        CorporationRecord {
            corporation_id: 98_001,
            name: "Trade Corp".into(),
            ticker: "TRD".into(),
            ceo_id: 90_001,
            tax_rate: 0.05,
            master_wallet: IskCents::from_isk(50_000),
            hq_station_id: Some(60_003_760),
            ..CorporationRecord::default()
        },
    );

    // JournalMeta builder + corp division clamping ((account_key - 999).clamp(1, 7))
    let meta_div3 = JournalMeta::new(ref_types::CORPORATION_ACCOUNT_WITHDRAWAL, 98_001, 90_001, 1_704_067_200)
        .with_context(JournalContext::Corporation, 98_001)
        .with_account_key(1002);
    assert_eq!(meta_div3.account_key, 1002);
    assert_eq!(meta_div3.context, JournalContext::Corporation);
    assert_eq!(meta_div3.context_id, 98_001);

    let eid = snap.append_journal_entry(JournalEntryInput {
        owner_id: 98_001,
        entry_type: "CorpWithdrawal",
        amount: IskCents::from_isk(-500),
        balance_after: IskCents::from_isk(49_500),
        reason: "Div 3 payout".into(),
        meta: meta_div3,
    });
    assert_eq!(snap.wallet_journal_timestamps[&eid], 1_704_067_200);
    assert_eq!(snap.corp_wallet_journals[&(98_001, 3)].len(), 1);
    assert_eq!(snap.corp_wallet_journals[&(98_001, 3)][0].amount, -500.0);

    // MarketFill with player buyer + player seller + sales tax
    let tx1 = snap.journal_market_fill(MarketFill {
        buyer_id: 90_001,
        seller_id: 90_002,
        type_id: 34,
        quantity: 10,
        price: IskCents::from_isk(100),
        location_id: 60_003_760,
        date_unix: 1_704_067_200,
        gross: IskCents::from_isk(1_000),
        buyer_balance_after: Some(IskCents::from_isk(9_000)),
        seller_net: IskCents::from_isk(950),
        seller_balance_after: Some(IskCents::from_isk(10_950)),
        sales_tax: IskCents::from_isk(50),
        tax_collector_id: 1_000_132,
        region_id: Some(10_000_002),
    });
    assert_eq!(snap.char_wallet_transactions[&90_001].len(), 1);
    assert!(snap.char_wallet_transactions[&90_001][0].is_buy);
    assert_eq!(snap.char_wallet_transactions[&90_002].len(), 1);
    assert!(!snap.char_wallet_transactions[&90_002][0].is_buy);
    assert!(snap.market_transactions.contains_key(&tx1));
    assert!(snap.market_transactions[&tx1].journal_ref_id > 0);

    // MarketFill with corp buyer & seller + region_id fallback from market_orders
    snap.market_orders.insert(
        1,
        MarketOrderRecord {
            order_id: 1,
            owner_id: 90_001,
            station_id: 60_008_494,
            region_id: 10_000_043,
            type_id: 34,
            is_bid: false,
            price: IskCents::from_isk(120),
            vol_remaining: 50,
            vol_entered: 50,
            min_volume: 1,
            is_npc: false,
            escrow: IskCents::ZERO,
        },
    );
    let _tx2 = snap.journal_market_fill(MarketFill {
        buyer_id: 98_001,
        seller_id: 98_001,
        type_id: 34,
        quantity: 30,
        price: IskCents::from_isk(140),
        location_id: 60_008_494,
        date_unix: 1_704_067_200,
        gross: IskCents::from_isk(4_200),
        buyer_balance_after: None,
        seller_net: IskCents::from_isk(4_200),
        seller_balance_after: Some(IskCents::from_isk(53_700)),
        sales_tax: IskCents::ZERO,
        tax_collector_id: 1_000_132,
        region_id: None,
    });
    assert_eq!(snap.corp_wallet_transactions[&(98_001, 1)].len(), 2);
    assert!(snap.market_history.contains_key(&(10_000_043, 34)));

    // Same-day aggregation on (10_000_002, 34): 10 @ 100.0 then 30 @ 140.0 -> avg = 130.0, vol = 40
    snap.record_market_trade_daily_summary(10_000_002, 34, 1_704_067_210, 140.0, 30);
    let day = &snap.market_history[&(10_000_002, 34)][0];
    assert_eq!(day.volume, 40);
    assert_eq!(day.order_count, 2);
    assert!((day.average - 130.0).abs() < 1e-9);
    assert_eq!(day.highest, 140.0);
    assert_eq!(day.lowest, 100.0);
    assert_eq!(day.open_price, Some(100.0));
    assert_eq!(day.close_price, Some(140.0));
}

#[test]
fn plex_vault_shared_account_balance_and_legacy_folding() {
    let mut snap = WorldStateSnapshot::default();
    snap.characters.insert(90_001, make_char(90_001, 7, 120));
    snap.characters.insert(90_002, make_char(90_002, 7, 80));

    assert_eq!(snap.account_of_character(90_001), Some(7));
    assert_eq!(snap.account_of_character(99_999), None);
    assert_eq!(snap.plex_vault_balance(90_001), 0);

    // Fold legacy per-character plex_balance into shared account_plex_vault
    snap.fold_legacy_plex_balances();
    assert_eq!(snap.plex_vault_balance(90_001), 200);
    assert_eq!(snap.plex_vault_balance(90_002), 200);
    assert_eq!(snap.characters[&90_001].plex_balance, 0);
    assert_eq!(snap.characters[&90_002].plex_balance, 0);

    // Adjust PLEX vault: add, subtract to 0, reject overdraft without mutating
    assert_eq!(snap.adjust_plex_vault(90_001, 50), Some(250));
    assert_eq!(snap.adjust_plex_vault(90_002, -251), None);
    assert_eq!(snap.plex_vault_balance(90_001), 250);
    assert_eq!(snap.adjust_plex_vault(90_002, -250), Some(0));
    assert_eq!(snap.adjust_plex_vault(99_999, 10), None);

    assert_eq!(unix_secs_to_date_str(0), "1970-01-01");
    assert_eq!(unix_secs_to_date_str(1_704_067_200), "2024-01-01");
}

#[test]
fn adjust_plex_vault_automatically_folds_legacy_balances() {
    let mut snap = WorldStateSnapshot::default();
    snap.characters.insert(90_001, make_char(90_001, 7, 120));
    snap.characters.insert(90_002, make_char(90_002, 7, 80));

    assert_eq!(snap.unified_plex_balance(90_001), 200);
    assert_eq!(snap.adjust_plex_vault(90_002, -150), Some(50));
    assert_eq!(snap.account_plex_vault.get(&7).copied(), Some(50));
    assert_eq!(snap.characters[&90_001].plex_balance, 0);
    assert_eq!(snap.characters[&90_002].plex_balance, 0);
    assert_eq!(snap.adjust_plex_vault(90_001, -51), None);
    assert_eq!(snap.unified_plex_balance(90_001), 50);
}

#[test]
fn journal_clamping_market_fill_fallbacks_daily_summary_edges_and_unified_plex() {
    let mut snap = WorldStateSnapshot::default();
    snap.characters.insert(90_001, make_char(90_001, 7, 120));
    snap.characters.insert(90_002, make_char(90_002, 7, 80));
    snap.characters.insert(90_003, make_char(90_003, 9, 50));
    snap.corporations.insert(
        98_001,
        CorporationRecord {
            corporation_id: 98_001,
            name: "Corp".into(),
            ticker: "CRP".into(),
            ..Default::default()
        },
    );

    // JournalContext::default & JournalMeta::default
    assert_eq!(JournalContext::default(), JournalContext::None);
    assert_eq!(JournalMeta::default().ref_type_id, 0);

    // Character journal entry does NOT populate corp_wallet_journals
    let char_eid = snap.append_journal_entry(JournalEntryInput {
        owner_id: 90_001,
        entry_type: "PlayerDonation",
        amount: IskCents::from_isk(100),
        balance_after: IskCents::from_isk(10_100),
        reason: "Gift".into(),
        meta: JournalMeta::new(ref_types::PLAYER_DONATION, 90_002, 90_001, 100),
    });
    assert!(snap.wallet_journal_meta.contains_key(&char_eid));
    assert!(snap.corp_wallet_journals.is_empty());

    // Corporation account_key clamping: < 1000 -> div 1, > 1006 -> div 7
    snap.append_journal_entry(JournalEntryInput {
        owner_id: 98_001,
        entry_type: "LowKey",
        amount: IskCents::from_isk(10),
        balance_after: IskCents::from_isk(10),
        reason: "Clamp low".into(),
        meta: JournalMeta::new(1, 90_001, 98_001, 100).with_account_key(500),
    });
    snap.append_journal_entry(JournalEntryInput {
        owner_id: 98_001,
        entry_type: "HighKey",
        amount: IskCents::from_isk(20),
        balance_after: IskCents::from_isk(30),
        reason: "Clamp high".into(),
        meta: JournalMeta::new(1, 90_001, 98_001, 100).with_account_key(1099),
    });
    assert_eq!(snap.corp_wallet_journals[&(98_001, 1)].len(), 1);
    assert_eq!(snap.corp_wallet_journals[&(98_001, 7)].len(), 1);

    // MarketFill with neither player buyer nor seller balance, unknown entities, qty <= 0 -> 1, fallback region 10_000_002
    let tx_npc = snap.journal_market_fill(MarketFill {
        buyer_id: 1_000_035,
        seller_id: 1_000_036,
        type_id: 35,
        quantity: 0,
        price: IskCents::from_isk(25),
        location_id: 60_003_760,
        date_unix: 1_709_164_800, // 2024-02-29
        gross: IskCents::from_isk(25),
        buyer_balance_after: None,
        seller_net: IskCents::from_isk(25),
        seller_balance_after: None,
        sales_tax: IskCents::ZERO,
        tax_collector_id: 1_000_132,
        region_id: None,
    });
    let tx_rec = snap.market_transactions.get(&tx_npc).unwrap();
    assert_eq!(tx_rec.journal_ref_id, 0);
    assert_eq!(snap.market_history[&(10_000_002, 35)][0].date, "2024-02-29");
    assert_eq!(snap.market_history[&(10_000_002, 35)][0].volume, 1);

    // record_market_trade_daily_summary: pre-existing day with open_price = None and new_vol == 0 guard + lower/higher bounds
    snap.market_history.insert(
        (10_000_002, 36),
        vec![crate::MarketHistoryDayRecord {
            date: "2024-03-01".into(),
            average: 50.0,
            highest: 60.0,
            lowest: 40.0,
            order_count: 0,
            volume: 0,
            open_price: None,
            close_price: None,
        }],
    );
    snap.record_market_trade_daily_summary(10_000_002, 36, 1_709_251_200, 45.0, 0);
    let d36 = &snap.market_history[&(10_000_002, 36)][0];
    assert_eq!(d36.average, 50.0);
    assert_eq!(d36.open_price, Some(50.0));
    assert_eq!(d36.close_price, Some(45.0));
    assert_eq!(d36.highest, 60.0);
    assert_eq!(d36.lowest, 40.0);

    snap.record_market_trade_daily_summary(10_000_002, 36, 1_709_251_200, 30.0, 10);
    assert_eq!(snap.market_history[&(10_000_002, 36)][0].lowest, 30.0);
    snap.record_market_trade_daily_summary(10_000_002, 36, 1_709_251_200, 80.0, 10);
    assert_eq!(snap.market_history[&(10_000_002, 36)][0].highest, 80.0);

    // Leap / non-leap date edge cases
    assert_eq!(unix_secs_to_date_str(951_782_400), "2000-02-29");
    assert_eq!(unix_secs_to_date_str(1_740_700_800), "2025-02-28");
    assert_eq!(unix_secs_to_date_str(1_740_787_200), "2025-03-01");

    // Unified PLEX vault methods
    assert_eq!(snap.unified_plex_balance_for_user(7), 200);
    assert_eq!(snap.unified_plex_balance_for_user(9), 50);
    assert_eq!(snap.unified_plex_balance(90_001), 200);
    assert_eq!(snap.unified_plex_balance(99_999), 0);

    // Overdraft does NOT fold legacy balances or mutate state
    assert_eq!(snap.adjust_unified_plex_for_user(7, -201), None);
    assert_eq!(snap.characters[&90_001].plex_balance, 120);
    assert_eq!(snap.characters[&90_002].plex_balance, 80);
    assert!(!snap.account_plex_vault.contains_key(&7));

    // Unknown char returns None
    assert_eq!(snap.adjust_unified_plex(99_999, 10), None);

    // Successful unified adjust folds legacy balances for user 7 only
    assert_eq!(snap.adjust_unified_plex(90_001, -50), Some(150));
    assert_eq!(snap.characters[&90_001].plex_balance, 0);
    assert_eq!(snap.characters[&90_002].plex_balance, 0);
    assert_eq!(snap.characters[&90_003].plex_balance, 50);
    assert_eq!(snap.account_plex_vault[&7], 150);

    // Overflow checks on adjust_unified_plex_for_user & adjust_plex_vault
    assert_eq!(snap.adjust_unified_plex_for_user(7, i64::MAX), None);
    snap.account_plex_vault.insert(7, i64::MAX);
    assert_eq!(snap.adjust_plex_vault(90_001, 1), None);
    snap.characters.get_mut(&90_003).unwrap().plex_balance = i64::MAX;
    snap.account_plex_vault.insert(9, 1);
    assert_eq!(snap.adjust_unified_plex_for_user(9, 0), None);
}
