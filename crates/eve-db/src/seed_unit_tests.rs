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
    });
}

