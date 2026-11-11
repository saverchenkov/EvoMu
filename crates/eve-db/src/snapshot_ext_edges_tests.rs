//! Edge-case behavioral tests for `snapshot_ext.rs`.

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

fn sample_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    qty: i32,
) -> InventoryItemRecord {
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

fn sample_job(job_id: i64, blueprint_item_id: i64, status: &str) -> IndustryJobRecord {
    IndustryJobRecord {
        job_id,
        installer_id: 100,
        facility_id: 60_003_760,
        blueprint_item_id,
        blueprint_type_id: 681,
        activity_id: 1,
        runs: 1,
        product_type_id: 587,
        status: status.into(),
        duration_seconds: 60,
        start_unix: 100,
        blueprint_location_id: 60_003_760,
        output_location_id: 60_003_760,
        cost: None,
        licensed_runs: None,
        probability: None,
        completed_unix: None,
        completed_character_id: None,
    }
}

fn sample_corp(corp_id: i64, ceo_id: i64, master_wallet: IskCents) -> CorporationRecord {
    CorporationRecord {
        corporation_id: corp_id,
        name: format!("Corp {corp_id}"),
        ticker: "TST".to_string(),
        ceo_id,
        alliance_id: None,
        tax_rate: 0.05,
        master_wallet,
        members: HashSet::from([ceo_id]),
        member_roles: HashMap::new(),
        applications: HashMap::new(),
        wallet_divisions: CorporationRecord::default_wallet_divisions(master_wallet),
        description: String::new(),
        url: String::new(),
        hq_station_id: Some(60_003_760),
    }
}

#[test]
fn item_locking_handles_finished_contracts_jobs_trades_and_raffles() {
    let mut snap = WorldStateSnapshot::default();

    // Terminal contract statuses and Delivered industry job do NOT lock items.
    snap.contracts.insert(
        1,
        ContractRecord {
            contract_id: 1,
            status: "Completed".into(),
            item_ids: vec![10],
            ..Default::default()
        },
    );
    snap.contracts.insert(
        2,
        ContractRecord {
            contract_id: 2,
            status: "Expired".into(),
            item_ids: vec![11],
            ..Default::default()
        },
    );
    snap.industry_jobs.insert(3, sample_job(3, 12, "Delivered"));
    assert!(!snap.is_item_locked_in_contract_or_job(10));
    assert!(!snap.is_item_locked_in_contract_or_job(11));
    assert!(!snap.is_item_locked_in_contract_or_job(12));

    // Paused industry job DOES lock.
    snap.industry_jobs.insert(4, sample_job(4, 13, "Paused"));
    assert!(snap.is_item_locked_in_contract_or_job(13));

    // Direct trade (-73): items_a & items_b lock unless exempted, completed, or cancelled.
    snap.owner_notes.insert(
        101,
        OwnerNoteRecord {
            note_id: 101,
            character_id: -73,
            item_id: 0,
            label: "trade".into(),
            note: r#"{"trade_id":701,"items_a":[[20,1]],"items_b":[[21,5]],"completed":false,"cancelled":false}"#.into(),
            timestamp_unix: 1,
        },
    );
    snap.owner_notes.insert(
        102,
        OwnerNoteRecord {
            note_id: 102,
            character_id: -73,
            item_id: 0,
            label: "trade_done".into(),
            note: r#"{"trade_id":702,"items_a":[[22,1]],"items_b":[],"completed":true,"cancelled":false}"#.into(),
            timestamp_unix: 1,
        },
    );
    snap.owner_notes.insert(
        103,
        OwnerNoteRecord {
            note_id: 103,
            character_id: -73,
            item_id: 0,
            label: "trade_cancel".into(),
            note: r#"{"trade_id":703,"items_a":[],"items_b":[[23,1]],"completed":false,"cancelled":true}"#.into(),
            timestamp_unix: 1,
        },
    );
    snap.owner_notes.insert(
        104,
        OwnerNoteRecord {
            note_id: 104,
            character_id: -73,
            item_id: 0,
            label: "bad_json".into(),
            note: "{not-json".into(),
            timestamp_unix: 1,
        },
    );
    assert!(snap.is_item_locked_excluding_trade(20, None));
    assert!(snap.is_item_locked_excluding_trade(21, Some(999)));
    assert!(!snap.is_item_locked_excluding_trade(20, Some(701)));
    assert!(!snap.is_item_locked_excluding_trade(21, Some(701)));
    assert!(!snap.is_item_locked_excluding_trade(22, None));
    assert!(!snap.is_item_locked_excluding_trade(23, None));

    // HyperNet raffles (-74): active raffle locks, won/expired/bad-json do not.
    snap.owner_notes.insert(
        201,
        OwnerNoteRecord {
            note_id: 201,
            character_id: -74,
            item_id: 0,
            label: "raffle_active".into(),
            note: r#"{"item_id":30,"winner_id":null,"expired":false}"#.into(),
            timestamp_unix: 1,
        },
    );
    snap.owner_notes.insert(
        202,
        OwnerNoteRecord {
            note_id: 202,
            character_id: -74,
            item_id: 0,
            label: "raffle_won".into(),
            note: r#"{"item_id":31,"winner_id":999,"expired":false}"#.into(),
            timestamp_unix: 1,
        },
    );
    snap.owner_notes.insert(
        203,
        OwnerNoteRecord {
            note_id: 203,
            character_id: -74,
            item_id: 0,
            label: "raffle_exp".into(),
            note: r#"{"item_id":32,"winner_id":null,"expired":true}"#.into(),
            timestamp_unix: 1,
        },
    );
    snap.owner_notes.insert(
        204,
        OwnerNoteRecord {
            note_id: 204,
            character_id: -74,
            item_id: 0,
            label: "raffle_bad".into(),
            note: "corrupt".into(),
            timestamp_unix: 1,
        },
    );
    assert!(snap.is_item_locked_excluding_trade(30, Some(701)));
    assert!(!snap.is_item_locked_excluding_trade(31, None));
    assert!(!snap.is_item_locked_excluding_trade(32, None));
}

#[test]
fn ship_colocation_saved_controller_and_consumability_edge_cases() {
    let mut snap = WorldStateSnapshot::default();
    let char_id = 500;
    let mut pilot = sample_character(char_id, 1, 98_000_001, None);
    pilot.station_id = Some(60_003_760);
    pilot.structure_id = None;
    pilot.solar_system_id = 30_000_142;
    pilot.active_ship_id = 200_000_001;
    snap.characters.insert(char_id, pilot);

    // Non-positive item_id is never treated as a boarded/saved ship even if session matches.
    assert!(!snap.is_character_boarded_or_saved_ship(char_id, Some(0), 0));
    assert!(!snap.is_character_boarded_or_saved_ship(char_id, Some(-5), -5));
    assert!(!snap.is_structure_controller_saved_ship(0));

    // Invalid JSON in -7_700_001 does not panic or falsely match.
    snap.item_custom_names
        .insert(-7_700_001, "{invalid_json".into());
    assert!(!snap.is_structure_controller_saved_ship(200_000_002));

    // Saved ship without an active controller in active_pilot_by_structure does NOT qualify;
    // saved ship WITH active controller DOES qualify.
    snap.item_custom_names.insert(
        -7_700_001,
        r#"{"active_pilot_by_structure":{"9001":500},"previous_ship_by_structure":{"9001":200000002,"9002":200000003}}"#.into(),
    );
    assert!(snap.is_structure_controller_saved_ship(200_000_002));
    assert!(!snap.is_structure_controller_saved_ship(200_000_003));
    assert!(snap.is_character_boarded_or_saved_ship(char_id, None, 200_000_002));
    assert!(!snap.is_character_boarded_or_saved_ship(char_id, None, 200_000_003));

    // Colocation checks: unknown character in space, wrong owner, docked vs in-space mismatch.
    snap.items.insert(
        200_000_001,
        sample_item(200_000_001, 587, char_id, 60_003_760, InventoryFlag::Hangar, 1),
    );
    assert!(!snap.is_ship_colocated_with_character(999, 200_000_001, None));
    assert!(snap.is_ship_colocated_with_character(char_id, 200_000_001, Some(60_003_760)));
    assert!(!snap.is_ship_colocated_with_character(char_id, 200_000_001, Some(60_003_761)));

    // Wrong owner
    snap.items.insert(
        200_000_010,
        sample_item(200_000_010, 587, 999, 60_003_760, InventoryFlag::Hangar, 1),
    );
    assert!(!snap.is_ship_colocated_with_character(char_id, 200_000_010, Some(60_003_760)));

    // Undocked character: active ship in solar system is colocated; ship in wrong system is not.
    snap.items.get_mut(&200_000_001).unwrap().location_id = 30_000_142;
    assert!(snap.is_ship_colocated_with_character(char_id, 200_000_001, None));
    snap.items.get_mut(&200_000_001).unwrap().location_id = 30_000_144;
    assert!(!snap.is_ship_colocated_with_character(char_id, 200_000_001, None));

    // Re-dock ship to station and test is_item_consumable_by edges:
    snap.items.get_mut(&200_000_001).unwrap().location_id = 60_003_760;

    // 1) Item in ship cargo works while colocated & unlocked; fails when no active_ship_id
    let cargo_item = sample_item(300_000_001, 34, char_id, 200_000_001, InventoryFlag::Cargo, 10);
    assert!(snap.is_item_consumable_by(
        &cargo_item,
        char_id,
        Some(60_003_760),
        Some(200_000_001)
    ));
    assert!(!snap.is_item_consumable_by(&cargo_item, char_id, Some(60_003_760), None));

    // 2) Item in wrong ship slot (HiSlot0) is not consumable
    let hi_slot_item = sample_item(300_000_002, 34, char_id, 200_000_001, InventoryFlag::HiSlot0, 1);
    assert!(!snap.is_item_consumable_by(
        &hi_slot_item,
        char_id,
        Some(60_003_760),
        Some(200_000_001)
    ));

    // 3) If enclosing ship is locked in a contract, in_ship_cargo returns false
    snap.contracts.insert(
        50,
        ContractRecord {
            contract_id: 50,
            status: "Outstanding".into(),
            item_ids: vec![200_000_001],
            ..Default::default()
        },
    );
    assert!(!snap.is_item_consumable_by(
        &cargo_item,
        char_id,
        Some(60_003_760),
        Some(200_000_001)
    ));
    snap.contracts.remove(&50);

    // 4) Undocked character (`station_or_struct_id == None`) cannot consume items sitting in station hangar
    let hangar_item = sample_item(300_000_003, 34, char_id, 60_003_760, InventoryFlag::Hangar, 5);
    assert!(snap.is_item_consumable_by(
        &hangar_item,
        char_id,
        Some(60_003_760),
        Some(200_000_001)
    ));
    assert!(!snap.is_item_consumable_by(
        &hangar_item,
        char_id,
        None,
        Some(200_000_001)
    ));

    // 5) Locked item itself or saved controller ship is never consumable
    snap.owner_notes.insert(
        301,
        OwnerNoteRecord {
            note_id: 301,
            character_id: -74,
            item_id: 0,
            label: "raffle".into(),
            note: r#"{"item_id":300000003,"winner_id":null,"expired":false}"#.into(),
            timestamp_unix: 1,
        },
    );
    assert!(!snap.is_item_consumable_by(
        &hangar_item,
        char_id,
        Some(60_003_760),
        Some(200_000_001)
    ));
    let saved_ship_item = sample_item(200_000_002, 587, char_id, 60_003_760, InventoryFlag::Hangar, 1);
    assert!(!snap.is_item_consumable_by(
        &saved_ship_item,
        char_id,
        Some(60_003_760),
        Some(200_000_001)
    ));
}

#[test]
fn credit_contract_issuer_handles_corp_divisions_fallback_and_saturation() {
    let mut snap = WorldStateSnapshot::default();
    let char_id = 1001;
    let corp_id = 98_000_010;
    snap.characters
        .insert(char_id, sample_character(char_id, 1, corp_id, None));
    snap.corporations
        .insert(corp_id, sample_corp(corp_id, char_id, IskCents(500)));

    let corp_contract = ContractRecord {
        contract_id: 1,
        issuer_id: char_id,
        issuer_corp_id: corp_id,
        for_corp: true,
        ..Default::default()
    };

    // amount <= 0 is a no-op
    snap.credit_contract_issuer(&corp_contract, IskCents(0));
    snap.credit_contract_issuer(&corp_contract, IskCents(-50));
    assert_eq!(snap.corporations[&corp_id].master_wallet, IskCents(500));

    // for_corp with valid corp credits master_wallet AND division 1
    snap.credit_contract_issuer(&corp_contract, IskCents(250));
    assert_eq!(snap.corporations[&corp_id].master_wallet, IskCents(750));
    assert_eq!(
        snap.corporations[&corp_id].wallet_divisions[0].balance,
        IskCents(750)
    );
    assert_eq!(snap.characters[&char_id].balance, IskCents::from_isk(1_000));

    // Checked addition preserves existing balances on overflow rather than wrapping
    snap.credit_contract_issuer(&corp_contract, IskCents(i64::MAX));
    assert_eq!(snap.corporations[&corp_id].master_wallet, IskCents(750));
    assert_eq!(
        snap.corporations[&corp_id].wallet_divisions[0].balance,
        IskCents(750)
    );

    // for_corp with empty wallet_divisions auto-populates all 7 divisions seeded from master_wallet
    let empty_div_corp = 98_000_011;
    let mut corp_no_divs = sample_corp(empty_div_corp, char_id, IskCents(100));
    corp_no_divs.wallet_divisions.clear();
    snap.corporations.insert(empty_div_corp, corp_no_divs);
    let empty_div_contract = ContractRecord {
        contract_id: 4,
        issuer_id: char_id,
        issuer_corp_id: empty_div_corp,
        for_corp: true,
        ..Default::default()
    };
    snap.credit_contract_issuer(&empty_div_contract, IskCents(50));
    assert_eq!(snap.corporations[&empty_div_corp].master_wallet, IskCents(150));
    assert_eq!(snap.corporations[&empty_div_corp].wallet_divisions.len(), 7);
    assert_eq!(
        snap.corporations[&empty_div_corp].wallet_divisions[0].balance,
        IskCents(150)
    );

    // for_corp with missing corp falls back to issuer character wallet
    let missing_corp_contract = ContractRecord {
        contract_id: 2,
        issuer_id: char_id,
        issuer_corp_id: 98_999_999,
        for_corp: true,
        ..Default::default()
    };
    snap.credit_contract_issuer(&missing_corp_contract, IskCents(300));
    assert_eq!(
        snap.characters[&char_id].balance,
        IskCents(IskCents::from_isk(1_000).0 + 300)
    );

    // for_corp == false credits personal wallet directly
    let personal_contract = ContractRecord {
        contract_id: 3,
        issuer_id: char_id,
        issuer_corp_id: corp_id,
        for_corp: false,
        ..Default::default()
    };
    snap.credit_contract_issuer(&personal_contract, IskCents(125));
    assert_eq!(
        snap.characters[&char_id].balance,
        IskCents(IskCents::from_isk(1_000).0 + 425)
    );
}

#[test]
fn resolve_expired_contract_covers_auction_corp_winner_no_bids_and_guards() {
    let mut snap = WorldStateSnapshot::default();
    let issuer = 2001;
    let winner_corp = 98_000_020;
    let station_id = 60_003_760;
    snap.characters
        .insert(issuer, sample_character(issuer, 1, 98_000_001, None));
    snap.corporations
        .insert(winner_corp, sample_corp(winner_corp, issuer, IskCents(0)));

    // expiration_unix == 0 -> None; terminal statuses -> None
    snap.contracts.insert(
        10,
        ContractRecord {
            contract_id: 10,
            contract_type: "ItemExchange".into(),
            status: "Outstanding".into(),
            expiration_unix: 0,
            ..Default::default()
        },
    );
    assert_eq!(snap.resolve_expired_contract(10, 5_000), None);
    snap.contracts.insert(
        11,
        ContractRecord {
            contract_id: 11,
            contract_type: "Auction".into(),
            status: "Completed".into(),
            expiration_unix: 100,
            ..Default::default()
        },
    );
    assert_eq!(snap.resolve_expired_contract(11, 5_000), None);

    // Auction won by a corporation moves both listed & contract-contained items to CorpDeliveries and strips cosmetics
    snap.items.insert(
        9001,
        sample_item(9001, 587, issuer, 12, InventoryFlag::Locked, 1),
    );
    snap.items.insert(
        9002,
        sample_item(9002, 588, issuer, 12, InventoryFlag::Locked, 1),
    );
    snap.applied_ship_skins.insert(9001, 12345);
    snap.service_state_blobs
        .insert("contract_orig_flag:12:9001".into(), "4".into());
    snap.service_state_blobs
        .insert("contract_orig_flag:12:9002".into(), "4".into());
    snap.service_state_blobs
        .insert("contract_deposit:12".into(), "777".into());
    snap.contracts.insert(
        12,
        ContractRecord {
            contract_id: 12,
            issuer_id: issuer,
            contract_type: "Auction".into(),
            status: "Outstanding".into(),
            start_station_id: station_id,
            item_ids: vec![9001],
            expiration_unix: 1_000,
            current_bid: IskCents(5_000),
            current_bidder_id: Some(winner_corp),
            ..Default::default()
        },
    );
    assert_eq!(snap.resolve_expired_contract(12, 1_500), Some("Completed"));
    assert_eq!(snap.contracts[&12].acceptor_id, Some(winner_corp));
    assert_eq!(snap.items[&9001].owner_id, winner_corp);
    assert_eq!(snap.items[&9001].location_id, station_id);
    assert_eq!(snap.items[&9001].flag, InventoryFlag::CorpDeliveries);
    assert_eq!(snap.items[&9002].owner_id, winner_corp);
    assert_eq!(snap.items[&9002].flag, InventoryFlag::CorpDeliveries);
    assert!(!snap.applied_ship_skins.contains_key(&9001));
    assert!(!snap.service_state_blobs.contains_key("contract_deposit:12"));
    assert_eq!(
        snap.characters[&issuer].balance,
        IskCents(IskCents::from_isk(1_000).0 + 5_000 + 777)
    );

    // Auction with zero bids expires, restores original flag (unless Locked -> Hangar), ignores non-numeric deposit, credits corp if for_corp
    snap.corporations
        .insert(98_000_030, sample_corp(98_000_030, issuer, IskCents(100)));
    snap.items.insert(
        9003,
        sample_item(9003, 587, issuer, 13, InventoryFlag::Locked, 1),
    );
    snap.items.insert(
        9004,
        sample_item(9004, 588, issuer, 13, InventoryFlag::Locked, 1),
    );
    snap.service_state_blobs.insert(
        "contract_orig_flag:13:9003".into(),
        (InventoryFlag::ShipHangar as u16).to_string(),
    );
    snap.service_state_blobs.insert(
        "contract_orig_flag:13:9004".into(),
        (InventoryFlag::Locked as u16).to_string(),
    );
    snap.service_state_blobs
        .insert("contract_deposit:13".into(), "corrupt".into());
    snap.contracts.insert(
        13,
        ContractRecord {
            contract_id: 13,
            issuer_id: issuer,
            issuer_corp_id: 98_000_030,
            for_corp: true,
            contract_type: "Auction".into(),
            status: "Outstanding".into(),
            start_station_id: station_id,
            reward: IskCents(200),
            item_ids: vec![9003],
            expiration_unix: 1_000,
            current_bid: IskCents(0),
            current_bidder_id: None,
            ..Default::default()
        },
    );
    assert_eq!(snap.resolve_expired_contract(13, 1_500), Some("Expired"));
    assert_eq!(snap.items[&9003].owner_id, 98_000_030);
    assert_eq!(snap.items[&9003].flag, InventoryFlag::ShipHangar);
    assert_eq!(snap.items[&9004].flag, InventoryFlag::Hangar);
    assert_eq!(snap.corporations[&98_000_030].master_wallet, IskCents(300));

    // InProgress non-Courier contract returns Some("Failed") without mutating status
    snap.contracts.insert(
        14,
        ContractRecord {
            contract_id: 14,
            contract_type: "ItemExchange".into(),
            status: "InProgress".into(),
            expiration_unix: 1_000,
            ..Default::default()
        },
    );
    assert_eq!(snap.resolve_expired_contract(14, 1_500), Some("Failed"));
    assert_eq!(snap.contracts[&14].status, "InProgress");
}

#[test]
fn fail_courier_contract_handles_ship_cargo_fallback_acceptor_and_guards() {
    let mut snap = WorldStateSnapshot::default();
    let issuer = 3001;
    let station_id = 60_003_760;
    snap.characters
        .insert(issuer, sample_character(issuer, 1, 98_000_001, None));

    // Missing, wrong type, and wrong status return false
    assert!(!snap.fail_courier_contract(999, 1_000));
    snap.contracts.insert(
        20,
        ContractRecord {
            contract_id: 20,
            contract_type: "Courier".into(),
            status: "Outstanding".into(),
            ..Default::default()
        },
    );
    assert!(!snap.fail_courier_contract(20, 1_000));

    // Active courier with acceptor_id == None falls back to issuer_entity_id,
    // non-wrap item inside an item (ship) receives InventoryFlag::Cargo,
    // deposit + collateral + reward credited.
    snap.items.insert(
        9500,
        sample_item(9500, 587, 7777, station_id, InventoryFlag::Hangar, 1),
    );
    snap.items.insert(
        9501,
        sample_item(9501, 34, 7777, 9500, InventoryFlag::Cargo, 10),
    );
    snap.service_state_blobs
        .insert("contract_deposit:21".into(), "150".into());
    snap.contracts.insert(
        21,
        ContractRecord {
            contract_id: 21,
            issuer_id: issuer,
            acceptor_id: None,
            contract_type: "Courier".into(),
            status: "InProgress".into(),
            start_station_id: station_id,
            reward: IskCents(1_000),
            collateral: IskCents(2_000),
            item_ids: vec![9501],
            ..Default::default()
        },
    );
    assert!(snap.fail_courier_contract(21, 5_000));
    assert_eq!(snap.contracts[&21].status, "Failed");
    assert_eq!(snap.items[&9501].owner_id, issuer);
    assert_eq!(snap.items[&9501].flag, InventoryFlag::Cargo);
    assert_eq!(
        snap.characters[&issuer].balance,
        IskCents(IskCents::from_isk(1_000).0 + 3_150)
    );
}

#[test]
fn id_allocation_reconciliation_cosmetic_stripping_and_date_formatting() {
    let mut snap = WorldStateSnapshot::default();
    // Notification above floor returns floor + 1 and advances monotonically
    let nid1 = snap.allocate_notification_id_above(1_000_000);
    assert_eq!(nid1, 1_000_001);
    let nid2 = snap.allocate_notification_id_above(900_000);
    assert_eq!(nid2, 1_000_002);
    let rid1 = snap.allocate_report_id();
    let rid2 = snap.allocate_report_id();
    assert_eq!(rid2, rid1 + 1);

    // Reconcile ignores >= 900_000_000 entity IDs and <= 98_000_000 corp IDs,
    // while picking up -79 notifications and -78 reports from owner_notes.
    snap.items.insert(
        950_000_000,
        sample_item(950_000_000, 34, 1, 60_003_760, InventoryFlag::Hangar, 1),
    );
    snap.items.insert(
        250_000_123,
        sample_item(250_000_123, 34, 1, 60_003_760, InventoryFlag::Hangar, 1),
    );
    snap.corporations
        .insert(1_000_125, sample_corp(1_000_125, 1, IskCents(0)));
    snap.owner_notes.insert(
        1,
        OwnerNoteRecord {
            note_id: -(7_900_000_000_000 + 2_500_000),
            character_id: -79,
            item_id: 0,
            label: "notif".into(),
            note: String::new(),
            timestamp_unix: 1,
        },
    );
    snap.owner_notes.insert(
        2,
        OwnerNoteRecord {
            note_id: 780_000_000 + 888_888,
            character_id: -78,
            item_id: 0,
            label: "report".into(),
            note: String::new(),
            timestamp_unix: 1,
        },
    );
    snap.reconcile_id_counters();
    assert_eq!(snap.alloc_entity_id(), 250_000_124);
    assert_eq!(snap.allocate_notification_id(), 2_500_001);
    assert_eq!(snap.allocate_report_id(), 888_889);
    // NPC corp <= 98_000_000 did not bump next_corporation_id above player corp floor
    let cid = snap.allocate_corporation_id();
    assert!(cid >= 98_000_001);

    // strip_ship_cosmetics: empty / negative IDs are no-ops; valid ID strips skinr & logos while keeping other ships intact
    snap.applied_ship_skins.insert(501, 10);
    snap.applied_ship_skins.insert(502, 20);
    snap.quasar_state_blobs.insert(
        "quasar_cosmetics_skinr".into(),
        r#"{"applied_thirdparty_skins":{"501":{"skin":"a"},"502":{"skin":"b"}}}"#.into(),
    );
    snap.quasar_state_blobs.insert(
        "quasar_cosmetics_live".into(),
        r#"{"ship_logos":{"501":["corp"],"502":["alliance"]}}"#.into(),
    );
    snap.strip_ship_cosmetics(&[]);
    snap.strip_ship_cosmetics(&[-10]);
    assert!(snap.applied_ship_skins.contains_key(&501));

    snap.strip_ship_cosmetics(&[501, -99]);
    assert!(!snap.applied_ship_skins.contains_key(&501));
    assert_eq!(snap.applied_ship_skins.get(&502), Some(&20));
    let skinr = serde_json::from_str::<serde_json::Value>(
        &snap.quasar_state_blobs["quasar_cosmetics_skinr"],
    )
    .unwrap();
    assert!(skinr["applied_thirdparty_skins"].get("501").is_none());
    assert!(skinr["applied_thirdparty_skins"].get("502").is_some());
    let live = serde_json::from_str::<serde_json::Value>(
        &snap.quasar_state_blobs["quasar_cosmetics_live"],
    )
    .unwrap();
    assert!(live["ship_logos"].get("501").is_none());
    assert!(live["ship_logos"].get("502").is_some());

    // Date formatting across century transitions (2000-02-29, 2000-03-01, 2024-03-01, 2100-03-01)
    assert_eq!(unix_secs_to_date_string(951_782_400), "2000-02-29");
    assert_eq!(unix_secs_to_date_string(951_868_800), "2000-03-01");
    assert_eq!(unix_secs_to_date_string(1_709_251_200), "2024-03-01");
    assert_eq!(unix_secs_to_date_string(4_107_542_400), "2100-03-01");

    // title_id_to_bit, seed_default_corp_titles, and effective_corp_roles power-of-two title bitmasks (#787)
    assert_eq!(title_id_to_bit(0), 0);
    assert_eq!(title_id_to_bit(-1), 0);
    assert_eq!(title_id_to_bit(-64), 0);
    assert_eq!(title_id_to_bit(1), 1);
    assert_eq!(title_id_to_bit(2), 2);
    assert_eq!(title_id_to_bit(4), 4);
    assert_eq!(title_id_to_bit(32_768), 32_768);
    assert_eq!(title_id_to_bit(1 << 20), 1 << 20);
    assert_eq!(title_id_to_bit(3), 1u64 << 2);
    assert_eq!(title_id_to_bit(5), 1u64 << 4);
    assert_eq!(title_id_to_bit(63), 1u64 << 62);
    assert_eq!(title_id_to_bit(64), 64);
    assert_eq!(title_id_to_bit(65), 0);
    assert_eq!(title_id_to_bit(100), 0);

    let title_corp = 98_555_001_i64;
    let title_member = 90_555_002_i64;
    snap.corporations.insert(
        title_corp,
        crate::types::CorporationRecord {
            corporation_id: title_corp,
            name: "Title Corp".into(),
            ticker: "TITL".into(),
            ceo_id: 90_555_001,
            alliance_id: None,
            tax_rate: 0.0,
            url: String::new(),
            description: String::new(),
            hq_station_id: Some(60_003_760),
            members: [90_555_001, title_member].into_iter().collect(),
            member_roles: HashMap::new(),
            master_wallet: Default::default(),
            applications: Default::default(),
            wallet_divisions: Default::default(),
        },
    );
    snap.corp_governance
        .entry(title_corp)
        .or_default()
        .titles
        .insert(4, ("Existing4".into(), 128));
    snap.corp_titles.entry(title_corp).or_default().push(CorpTitleEntry {
        title_id: 4,
        name: "Existing4".into(),
        roles: vec!["RolePersonnelManager".into()],
        ..Default::default()
    });
    snap.seed_default_corp_titles(title_corp);
    assert_eq!(snap.corp_governance[&title_corp].titles.len(), 16);
    assert_eq!(
        snap.corp_governance[&title_corp].titles.get(&4),
        Some(&("Existing4".into(), 128))
    );
    let seeded_entries = &snap.corp_titles[&title_corp];
    assert_eq!(seeded_entries.len(), 16);
    let expected_ids: Vec<i64> = (0..16u32).map(|i| 1i64 << i).collect();
    let actual_ids: Vec<i64> = seeded_entries.iter().map(|t| t.title_id).collect();
    assert_eq!(actual_ids, expected_ids);
    assert_eq!(
        seeded_entries.iter().find(|t| t.title_id == 4).map(|t| t.name.as_str()),
        Some("Existing4")
    );

    // effective_corp_roles resolves power-of-two title IDs (> 64, e.g. 256 and 512) via title_id_to_bit
    snap.corp_governance
        .get_mut(&title_corp)
        .unwrap()
        .titles
        .insert(256, ("Bit256".into(), 1024));
    snap.corp_governance
        .get_mut(&title_corp)
        .unwrap()
        .member_titles
        .insert(title_member, 256 | 512);
    if let Some(t512) = snap
        .corp_titles
        .get_mut(&title_corp)
        .unwrap()
        .iter_mut()
        .find(|t| t.title_id == 512)
    {
        t512.roles = vec!["Director".into()];
    }
    let (is_ceo, eff_roles) =
        snap.effective_corp_roles_at(title_corp, title_member, None, 0, 1_800_000_000);
    assert!(!is_ceo);
    assert_ne!(eff_roles & crate::corp_roles::ROLE_DIRECTOR, 0);
    assert_ne!(eff_roles & 1024, 0);
}
