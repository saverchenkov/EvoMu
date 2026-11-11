//! Unit and mutation-killing tests for `seed.rs` (`create_demo_snapshot`, `populate_demo_snapshot`, `with_demo_seed`).

use super::*;

#[test]
fn create_demo_snapshot_populates_accounts_characters_items_skills_markets_and_fw() {
    let snap = WorldRepository::create_demo_snapshot();
    assert_eq!(snap.next_entity_id, 200_000_000);
    assert_eq!(snap.wal_seq, 0);
    assert!(!snap.markets_seeded);

    // Accounts
    assert_eq!(snap.accounts.len(), 2);
    let demo_acc = &snap.accounts["demo"];
    assert_eq!(demo_acc.user_id, 1);
    assert_eq!(demo_acc.username, "demo");
    let demo_salt = demo_acc.password_token.split('$').nth(4).unwrap();
    assert_eq!(
        demo_acc.password_token,
        WorldRepository::hash_password_with_salt("demo", demo_salt, "demo-secret")
    );
    assert_eq!(demo_acc.role, service_roles::ROLE_PLAYER_DEFAULT);
    assert!(demo_acc.banned_until_unix.is_none());
    assert!(demo_acc.ban_reason.is_none());

    let other_acc = &snap.accounts["other"];
    assert_eq!(other_acc.user_id, 2);
    assert_eq!(other_acc.username, "other");
    let other_salt = other_acc.password_token.split('$').nth(4).unwrap();
    assert_eq!(
        other_acc.password_token,
        WorldRepository::hash_password_with_salt("other", other_salt, "other-secret")
    );

    // Characters & folded PLEX vault balances
    assert_eq!(snap.characters.len(), 2);
    let aura = &snap.characters[&90_000_001];
    assert_eq!(aura.user_id, 1);
    assert_eq!(aura.name, "Aura Capsuleer");
    assert_eq!(aura.corporation_id, 1_000_170);
    assert_eq!(aura.station_id, Some(60_003_760));
    assert_eq!(aura.solar_system_id, 30_000_142);
    assert_eq!(aura.constellation_id, 20_000_020);
    assert_eq!(aura.region_id, 10_000_002);
    assert_eq!(aura.active_ship_id, 100_000_001);
    assert_eq!(aura.balance, IskCents::from_isk(1_000_000_000));
    assert_eq!(aura.plex_balance, 0);
    assert_eq!(snap.account_plex_vault.get(&1), Some(&500));
    assert_eq!(aura.loyalty_points.get(&1_000_035), Some(&5_000));
    assert_eq!(aura.standings.get(&1_000_035), Some(&2.0));
    assert_eq!(aura.security_status, 0.5);
    assert_eq!(aura.clone_grade, 2);
    assert_eq!(aura.home_station_id, 60_003_760);
    assert_eq!(aura.race_id, 2);
    assert_eq!(aura.bloodline_id, 3);
    assert_eq!(aura.ancestry_id, 19);
    assert_eq!(aura.school_id, 14);
    assert_eq!(aura.skill_points, 15_000_000.0);
    assert_eq!(aura.free_skill_points, 50_000.0);
    assert_eq!(aura.create_date_unix, 1_704_067_200);
    assert_eq!(aura.version, 1);
    assert_eq!(aura.system_epoch, 1);

    let rival = &snap.characters[&90_000_002];
    assert_eq!(rival.user_id, 2);
    assert_eq!(rival.name, "Rival Pilot");
    assert_eq!(rival.corporation_id, 1_000_045);
    assert_eq!(rival.active_ship_id, 100_000_002);
    assert_eq!(rival.balance, IskCents::from_isk(50_000_000));
    assert_eq!(rival.plex_balance, 0);
    assert_eq!(snap.account_plex_vault.get(&2), Some(&100));
    assert_eq!(rival.clone_grade, 1);
    assert_eq!(rival.race_id, 1);
    assert_eq!(rival.bloodline_id, 1);
    assert_eq!(rival.ancestry_id, 11);
    assert_eq!(rival.school_id, 18);
    assert_eq!(rival.skill_points, 4_500_000.0);
    assert_eq!(rival.free_skill_points, 0.0);

    // Items (100_000_001..=100_000_012 => 12 items)
    assert_eq!(snap.items.len(), 12);
    assert_eq!(snap.items[&100_000_001].type_id, 587);
    assert_eq!(snap.items[&100_000_001].flag, InventoryFlag::Hangar);
    assert_eq!(snap.items[&100_000_005].type_id, 2889);
    assert_eq!(snap.items[&100_000_005].location_id, 100_000_001);
    assert_eq!(snap.items[&100_000_005].flag, InventoryFlag::HiSlot0);
    assert_eq!(snap.items[&100_000_006].location_id, 100_000_002);
    assert_eq!(snap.items[&100_000_008].type_id, 34);
    assert_eq!(snap.items[&100_000_008].quantity, 100_000);
    assert!(!snap.items[&100_000_008].singleton);

    // Market orders
    assert_eq!(snap.market_orders.len(), 2);
    assert_eq!(snap.market_orders[&700_000_001].type_id, 34);
    assert_eq!(snap.market_orders[&700_000_001].price, IskCents::from_isk(5));
    assert!(snap.market_orders[&700_000_001].is_npc);
    assert_eq!(snap.market_orders[&700_000_002].type_id, 438);
    assert_eq!(
        snap.market_orders[&700_000_002].price,
        IskCents::from_isk(550_000)
    );

    // System states & NPE & Skills & Corporations & FW
    assert_eq!(snap.system_states.len(), 5);
    assert_eq!(snap.system_states[&30_000_142].dbs_multiplier, 1.35);
    assert_eq!(snap.system_states[&30_000_142].military_index, 2);
    assert_eq!(snap.system_states[&30_000_142].strategic_index, 3);
    assert_eq!(snap.npe_progress[&90_000_001].node_graph_id, 473);
    assert_eq!(snap.skills[&90_000_001].len(), 15);
    assert_eq!(snap.skills[&90_000_002].len(), 15);
    assert_eq!(snap.skill_history[&90_000_001].len(), 15);
    assert_eq!(snap.skill_history[&90_000_001][0].event_type_id, 33);
    assert_eq!(snap.corporations[&98_000_001].ticker, "DEMO");
    assert_eq!(snap.corporations[&98_000_001].ceo_id, 90_000_001);
    assert_eq!(snap.fw_system_occupations.len(), 175);
}

#[test]
fn populate_demo_snapshot_fills_empty_and_preserves_existing_records() {
    let mut empty = WorldStateSnapshot::default();
    empty.next_entity_id = 100;
    WorldRepository::populate_demo_snapshot(&mut empty);
    assert_eq!(empty.next_entity_id, 200_000_000);
    assert_eq!(empty.accounts.len(), 2);
    assert_eq!(empty.characters.len(), 2);
    assert_eq!(empty.items.len(), 12);
    assert_eq!(empty.market_orders.len(), 2);
    assert_eq!(empty.corporations.len(), 1);
    assert_eq!(empty.system_states.len(), 5);
    assert_eq!(empty.npe_progress.len(), 1);

    // Modify an existing character and raise next_entity_id, then re-populate: existing values must not be overwritten
    empty
        .characters
        .get_mut(&90_000_001)
        .unwrap()
        .name = "Custom Name".to_string();
    empty.next_entity_id = 555_000_000;
    WorldRepository::populate_demo_snapshot(&mut empty);
    assert_eq!(empty.characters[&90_000_001].name, "Custom Name");
    assert_eq!(empty.next_entity_id, 555_000_000);
}

#[test]
fn with_demo_seed_initializes_working_sqlite_backed_repository() {
    let repo = WorldRepository::with_demo_seed();
    repo.with_snapshot(|snap| {
        assert_eq!(snap.accounts.len(), 2);
        assert_eq!(snap.characters.len(), 2);
        assert_eq!(snap.fw_system_occupations.len(), 175);
        assert_eq!(snap.corporations[&98_000_001].wallet_divisions.len(), 7);
    });
}

#[test]
fn demo_snapshot_starter_items_system_states_corp_roles_and_partial_populate() {
    let snap = WorldRepository::create_demo_snapshot();

    // Verify every seeded starter item 100_000_003..=100_000_012
    let expected_items = [
        (100_000_003i64, 438u32, 1i32, true),
        (100_000_004, 1236, 2, true),
        (100_000_007, 691, 1, true),
        (100_000_008, 34, 100_000, false),
        (100_000_009, 35, 50_000, false),
        (100_000_010, 36, 20_000, false),
        (100_000_011, 37, 5_000, false),
        (100_000_012, 1230, 10_000, false),
    ];
    for (id, tid, qty, single) in expected_items {
        let item = &snap.items[&id];
        assert_eq!(item.type_id, tid);
        assert_eq!(item.quantity, qty);
        assert_eq!(item.singleton, single);
        assert_eq!(item.owner_id, 90_000_001);
        assert_eq!(item.location_id, 60_003_760);
        assert_eq!(item.flag, InventoryFlag::Hangar);
    }

    // All 5 seeded systems
    for sys_id in [30_000_142u32, 30_000_144, 30_002_813, 30_002_904, 31_000_005] {
        let st = &snap.system_states[&sys_id];
        assert_eq!(st.solar_system_id, sys_id);
        assert_eq!(st.dbs_multiplier, 1.35);
        assert_eq!(st.industrial_index, 2);
        assert_eq!(st.strategic_index, 3);
    }

    // Demo corporation wallet divisions & member roles
    let corp = &snap.corporations[&98_000_001];
    assert_eq!(corp.master_wallet, IskCents::from_isk(100_000_000));
    assert_eq!(corp.wallet_divisions[0].balance, IskCents::from_isk(100_000_000));
    assert_eq!(corp.member_roles.get(&90_000_001), Some(&u64::MAX));
    assert_eq!(corp.member_roles.get(&90_000_002), Some(&0));
    assert!(corp.members.contains(&90_000_001));
    assert!(corp.members.contains(&90_000_002));

    // Starter skill history sorted ascending by skill_type_id
    let hist = &snap.skill_history[&90_000_001];
    assert!(hist.windows(2).all(|w| w[0].skill_type_id <= w[1].skill_type_id));
    assert_eq!(hist[0].log_date_ft, 133_485_408_000_000_000);

    // Partial populate_demo_snapshot preserves pre-existing entries across all maps while filling missing ones
    let mut partial = WorldStateSnapshot::default();
    partial.account_plex_vault.insert(1, 999);
    partial.items.insert(
        100_000_008,
        InventoryItemRecord {
            item_id: 100_000_008,
            type_id: 34,
            quantity: 7,
            ..Default::default()
        },
    );
    partial.market_orders.insert(
        700_000_001,
        MarketOrderRecord {
            order_id: 700_000_001,
            owner_id: 1,
            station_id: 60_003_760,
            region_id: 10_000_002,
            type_id: 34,
            is_bid: false,
            price: IskCents::from_isk(99),
            vol_entered: 1,
            vol_remaining: 1,
            min_volume: 1,
            is_npc: false,
            escrow: IskCents::ZERO,
        },
    );
    WorldRepository::populate_demo_snapshot(&mut partial);
    assert_eq!(partial.account_plex_vault[&1], 999);
    assert_eq!(partial.account_plex_vault[&2], 100);
    assert_eq!(partial.items[&100_000_008].quantity, 7);
    assert_eq!(partial.items.len(), 12);
    assert_eq!(partial.market_orders[&700_000_001].price, IskCents::from_isk(99));
    assert_eq!(partial.market_orders.len(), 2);
}

