//! Targeted unit and mutation-killing tests for `snapshot_ext.rs`.

use super::*;

fn sample_character(char_id: i64, user_id: u64, corp_id: i64, alliance_id: Option<i64>) -> CharacterRecord {
    CharacterRecord {
        character_id: char_id,
        user_id,
        name: format!("Pilot {char_id}"),
        corporation_id: corp_id,
        alliance_id,
        war_faction_id: None,
        station_id: Some(60_003_760),
        structure_id: None,
        solar_system_id: 30_000_142,
        constellation_id: 20_000_020,
        region_id: 10_000_002,
        active_ship_id: 200_000_001,
        balance: IskCents::from_isk(1_000),
        plex_balance: 0,
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

fn sample_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, qty: i32) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity: qty,
        singleton: qty == 1,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

#[test]
fn starbase_defaults_market_history_helpers_and_date_formatting() {
    let sb = StarbaseDetailSettings::default();
    assert!(sb.allow_corporation_members);
    assert!(sb.allow_alliance_members);
    assert!(!sb.use_alliance_standings);
    assert!(!sb.attack_if_other_security_status_dropping);
    assert!(sb.attack_if_at_war);
    assert_eq!(sb.fuel_bay_view, "corp");
    assert_eq!(sb.fuel_bay_take, "corp");
    assert_eq!(sb.anchor, "corp");
    assert_eq!(sb.unanchor, "corp");
    assert_eq!(sb.online, "corp");
    assert_eq!(sb.offline, "corp");
    assert_eq!(sb.last_moon_process_unix, 0);

    let mut day = MarketHistoryDayRecord {
        date: "2026-01-15".into(),
        average: 100.0,
        highest: 125.0,
        lowest: 85.0,
        order_count: 4,
        volume: 50,
        open_price: None,
        close_price: None,
    };
    assert_eq!(day.open_isk(), 100.0);
    assert_eq!(day.high_isk(), 125.0);
    assert_eq!(day.low_isk(), 85.0);
    assert_eq!(day.close_isk(), 100.0);

    day.open_price = Some(92.5);
    day.close_price = Some(118.25);
    assert_eq!(day.open_isk(), 92.5);
    assert_eq!(day.close_isk(), 118.25);

    assert_eq!(unix_secs_to_date_string(0), "1970-01-01");
    assert_eq!(unix_secs_to_date_string(1_704_067_200), "2024-01-01");
    assert_eq!(unix_secs_to_date_string(1_700_000_000), "2023-11-14");
    assert_eq!(unix_secs_to_date_string(1_709_251_200), "2024-03-01");
}

#[test]
fn record_killmail_system_jump_and_system_kill_retention_boundaries() {
    let mut snap = WorldStateSnapshot::default();
    snap.characters.insert(90_001, sample_character(90_001, 1, 98_001, Some(99_001)));
    snap.characters.insert(90_002, sample_character(90_002, 2, 98_002, None));

    let km = KillmailRecord {
        kill_id: 501,
        solar_system_id: 30_000_142,
        victim_character_id: 90_001,
        victim_corp_id: 1_000_001,
        victim_alliance_id: None,
        ship_type_id: 587,
        attacker_character_id: 90_002,
        attacker_corp_id: 1_000_002,
        attacker_alliance_id: Some(99_999),
        total_value_isk: 1_000_000.0,
        killed_unix: 0,
    };
    snap.record_killmail(km, 1_750_000_123);
    let stored = &snap.killmails[&501];
    assert_eq!(stored.killed_unix, 1_750_000_123);
    assert_eq!(stored.victim_corp_id, 98_001);
    assert_eq!(stored.victim_alliance_id, Some(99_001));
    assert_eq!(stored.attacker_corp_id, 98_002);
    assert_eq!(stored.attacker_alliance_id, None);

    // Unknown pilot IDs preserve pre-populated structure/NPC affiliation
    let npc_km = KillmailRecord {
        kill_id: 502,
        solar_system_id: 30_000_142,
        victim_character_id: 0,
        victim_corp_id: 98_777,
        victim_alliance_id: Some(99_777),
        ship_type_id: 35832,
        attacker_character_id: 0,
        attacker_corp_id: 1_000_125,
        attacker_alliance_id: None,
        total_value_isk: 500_000_000.0,
        killed_unix: 0,
    };
    snap.record_killmail(npc_km, 1_750_000_200);
    assert_eq!(snap.killmails[&502].victim_corp_id, 98_777);
    assert_eq!(snap.killmails[&502].victim_alliance_id, Some(99_777));

    // Jump & Kill 86,400s exact boundary test
    snap.record_system_jump(30_000_142, 90_001, 10_000);
    snap.record_system_jump(30_000_142, 90_002, 10_001);
    snap.record_system_jump(30_000_142, 90_001, 10_000 + 86_400);
    assert_eq!(snap.system_jump_events.len(), 3);
    snap.record_system_jump(30_000_142, 90_001, 10_000 + 86_401);
    assert_eq!(snap.system_jump_events.len(), 3);
    assert_eq!(snap.system_jump_events[0].timestamp_unix, 10_001);

    snap.record_system_kill(30_000_142, 1, 2, 0, 20_000);
    snap.record_system_kill(30_000_142, 0, 5, 1, 20_001);
    snap.record_system_kill(30_000_142, 2, 0, 0, 20_000 + 86_400);
    assert_eq!(snap.system_kill_events.len(), 3);
    snap.record_system_kill(30_000_142, 1, 1, 1, 20_000 + 86_401);
    assert_eq!(snap.system_kill_events.len(), 3);
    assert_eq!(snap.system_kill_events[0].timestamp_unix, 20_001);
}

#[test]
fn item_locks_saved_ships_colocation_and_consumability_predicates() {
    let mut snap = WorldStateSnapshot::default();
    let char_id = 90_001;
    let ship_id = 200_001;
    let mut ch = sample_character(char_id, 1, 98_001, None);
    ch.active_ship_id = ship_id;
    snap.characters.insert(char_id, ch);
    snap.items.insert(
        ship_id,
        sample_item(ship_id, 587, char_id, 60_003_760, InventoryFlag::Hangar, 1),
    );

    // Structure controller saved ship check
    assert!(!snap.is_structure_controller_saved_ship(0));
    assert!(!snap.is_structure_controller_saved_ship(-5));
    snap.item_custom_names.insert(
        -7_700_001,
        r#"{"active_pilot_by_structure":{"1000000000100":90001},"previous_ship_by_structure":{"1000000000100":200099,"1000000000200":200100}}"#.into(),
    );
    assert!(snap.is_structure_controller_saved_ship(200_099));
    assert!(!snap.is_structure_controller_saved_ship(200_100)); // structure 200 has no active pilot
    assert!(snap.is_character_boarded_or_saved_ship(char_id, None, ship_id));
    assert!(snap.is_character_boarded_or_saved_ship(char_id, Some(200_050), 200_050));
    assert!(snap.is_character_boarded_or_saved_ship(char_id, None, 200_099));
    assert!(!snap.is_character_boarded_or_saved_ship(char_id, None, 0));

    // Co-location docked vs space
    assert!(snap.is_ship_colocated_with_character(char_id, ship_id, Some(60_003_760)));
    assert!(!snap.is_ship_colocated_with_character(char_id, ship_id, Some(60_003_761)));
    assert!(!snap.is_ship_colocated_with_character(99_999, ship_id, Some(60_003_760)));
    assert!(!snap.is_ship_colocated_with_character(char_id, 999_999, Some(60_003_760)));

    snap.items.get_mut(&ship_id).unwrap().location_id = 30_000_142;
    assert!(snap.is_ship_colocated_with_character(char_id, ship_id, None));
    snap.items.get_mut(&ship_id).unwrap().location_id = 60_003_760;

    // Consumable item checks: Hangar & Ship Cargo, Locked flag, Contract/Job escrow
    let hangar_ore = sample_item(200_010, 1230, char_id, 60_003_760, InventoryFlag::Hangar, 100);
    let cargo_ore = sample_item(200_011, 1230, char_id, ship_id, InventoryFlag::Cargo, 50);
    let locked_ore = sample_item(200_012, 1230, char_id, 60_003_760, InventoryFlag::Locked, 10);
    let zero_qty_ore = sample_item(200_013, 1230, char_id, 60_003_760, InventoryFlag::Hangar, 0);
    snap.items.insert(hangar_ore.item_id, hangar_ore.clone());
    snap.items.insert(cargo_ore.item_id, cargo_ore.clone());

    assert!(snap.is_item_consumable_by(&hangar_ore, char_id, Some(60_003_760), Some(ship_id)));
    assert!(snap.is_item_consumable_by(&cargo_ore, char_id, Some(60_003_760), Some(ship_id)));
    assert!(!snap.is_item_consumable_by(&locked_ore, char_id, Some(60_003_760), Some(ship_id)));
    assert!(!snap.is_item_consumable_by(&zero_qty_ore, char_id, Some(60_003_760), Some(ship_id)));
    assert!(!snap.is_item_consumable_by(&snap.items[&ship_id], char_id, Some(60_003_760), Some(ship_id)));

    // Lock in contract and industry job
    snap.contracts.insert(
        701,
        ContractRecord {
            contract_id: 701,
            issuer_id: char_id,
            issuer_corp_id: 98_001,
            contract_type: "ItemExchange".into(),
            status: "Outstanding".into(),
            start_station_id: 60_003_760,
            end_station_id: 60_003_760,
            item_ids: vec![hangar_ore.item_id],
            date_issued_unix: 100,
            expiration_unix: 200,
            ..ContractRecord::default()
        },
    );
    assert!(snap.is_item_locked_in_contract_or_job(hangar_ore.item_id));
    assert!(!snap.is_item_consumable_by(&hangar_ore, char_id, Some(60_003_760), Some(ship_id)));

    snap.industry_jobs.insert(
        801,
        IndustryJobRecord {
            job_id: 801,
            installer_id: char_id,
            facility_id: 60_003_760,
            activity_id: 1,
            blueprint_item_id: cargo_ore.item_id,
            blueprint_type_id: 1230,
            product_type_id: 34,
            runs: 1,
            start_unix: 100,
            duration_seconds: 100,
            status: "Paused".into(),
            licensed_runs: Some(10),
            probability: Some(1.0),
            cost: Some(100.0),
            ..IndustryJobRecord::default()
        },
    );
    assert!(snap.is_item_locked_in_contract_or_job(cargo_ore.item_id));
    assert!(!snap.is_item_consumable_by(&cargo_ore, char_id, Some(60_003_760), Some(ship_id)));
}

#[test]
fn contract_expiration_courier_failure_and_id_counter_reconciliation() {
    let mut snap = WorldStateSnapshot::default();
    snap.characters.insert(90_001, sample_character(90_001, 1, 98_001, None));
    snap.characters.insert(90_002, sample_character(90_002, 2, 98_002, None));
    snap.corporations.insert(
        98_001,
        CorporationRecord {
            corporation_id: 98_001,
            name: "Issuer Corp".into(),
            ticker: "ISSR".into(),
            ceo_id: 90_001,
            tax_rate: 0.05,
            master_wallet: IskCents::from_isk(1_000),
            hq_station_id: Some(60_003_760),
            members: HashSet::from([90_001]),
            ..CorporationRecord::default()
        },
    );

    // 1. Auction contract expiration with winning bidder + deposit for corp issuer
    snap.items.insert(300_001, sample_item(300_001, 34, 98_001, 701, InventoryFlag::Locked, 100));
    snap.service_state_blobs.insert("contract_deposit:701".into(), "5000".into()); // 50 ISK
    snap.contracts.insert(
        701,
        ContractRecord {
            contract_id: 701,
            issuer_id: 90_001,
            issuer_corp_id: 98_001,
            contract_type: "Auction".into(),
            status: "Outstanding".into(),
            for_corp: true,
            availability: "Public".into(),
            start_station_id: 60_003_760,
            end_station_id: 60_003_760,
            price: IskCents::from_isk(100),
            buyout: IskCents::from_isk(500),
            current_bid: IskCents::from_isk(250),
            current_bidder_id: Some(90_002),
            item_ids: vec![300_001],
            date_issued_unix: 100,
            expiration_unix: 500,
            ..ContractRecord::default()
        },
    );
    assert_eq!(snap.resolve_expired_contract(701, 499), None);
    assert_eq!(snap.resolve_expired_contract(701, 500), Some("Completed"));
    assert_eq!(snap.corporations[&98_001].master_wallet, IskCents::from_isk(1_300));
    assert_eq!(snap.corporations[&98_001].wallet_divisions.len(), 7);
    assert_eq!(snap.items[&300_001].owner_id, 90_002);
    assert_eq!(snap.items[&300_001].location_id, 60_003_760);
    assert_eq!(snap.items[&300_001].flag, InventoryFlag::Hangar);

    // 2. Courier contract failure unwraps plastic wrap (3468) inside acceptor's ship
    snap.items.insert(300_010, sample_item(300_010, 587, 90_002, 30_000_142, InventoryFlag::None, 1));
    snap.items.insert(300_011, sample_item(300_011, 3468, 90_002, 300_010, InventoryFlag::Cargo, 1));
    snap.items.insert(300_012, sample_item(300_012, 34, 90_001, 300_011, InventoryFlag::Locked, 50));
    snap.contracts.insert(
        702,
        ContractRecord {
            contract_id: 702,
            issuer_id: 90_001,
            issuer_corp_id: 98_001,
            acceptor_id: Some(90_002),
            contract_type: "Courier".into(),
            status: "InProgress".into(),
            availability: "Public".into(),
            start_station_id: 60_003_760,
            end_station_id: 60_003_760,
            reward: IskCents::from_isk(50),
            collateral: IskCents::from_isk(200),
            item_ids: vec![300_012],
            date_issued_unix: 100,
            expiration_unix: 600,
            ..ContractRecord::default()
        },
    );
    assert_eq!(snap.resolve_expired_contract(702, 600), Some("Failed"));
    assert!(!snap.items.contains_key(&300_011)); // plastic wrap removed
    assert_eq!(snap.items[&300_012].location_id, 300_010);
    assert_eq!(snap.items[&300_012].owner_id, 90_002);
    assert_eq!(snap.items[&300_012].flag, InventoryFlag::Cargo);
    assert_eq!(snap.characters[&90_001].balance, IskCents::from_isk(1_250));

    // 3. Monotonic ID allocation and reconciliation
    let eid = snap.alloc_entity_id();
    assert!(eid >= 200_000_001);
    let aid = snap.allocate_alliance_id();
    assert_eq!(aid, 99_000_001);
    let cid = snap.allocate_corporation_id();
    assert_eq!(cid, 98_100_001);
    let nid = snap.allocate_notification_id();
    assert_eq!(nid, 500_001);
    let rid = snap.allocate_report_id();
    assert_eq!(rid, 700_001);
    snap.reconcile_id_counters();
    assert!(snap.next_entity_id >= eid);
}
