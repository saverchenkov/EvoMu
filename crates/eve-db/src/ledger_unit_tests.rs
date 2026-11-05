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
    assert_eq!(snap.market_transactions.last().unwrap().transaction_id, tx1);
    assert!(snap.market_transactions.last().unwrap().journal_ref_id > 0);

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
