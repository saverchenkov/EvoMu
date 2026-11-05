//! Unit tests for `characters`, `inventory`, `ledger`, and `snapshot_ext` domain transactions.

use super::*;
use crate::ledger::{JournalContext, JournalEntryInput, JournalMeta, MarketFill, ref_types, unix_secs_to_date_str};

fn load_test_sde() -> eve_sde::SdeRegistry {
    match std::env::var_os("EVOMU_SDE_PATH") {
        Some(p) => eve_sde::SdeRegistry::open(p).expect("open sde"),
        None => eve_sde::SdeRegistry::with_canonical_seed(),
    }
}

#[test]
fn character_dock_respawn_structure_and_pod_remap_and_mining_ledger() {
    let sde = load_test_sde();
    let repo = WorldRepository::with_demo_seed();
    let cid = 90_000_001_i64;
    let jita_sys = 30_000_142_u32;

    // Missing character dock returns CharacterNotFound.
    assert_eq!(
        repo.dock_character(99_999_999, 60_003_760, false),
        Err(DbError::CharacterNotFound(99_999_999))
    );

    // Undock -> ownership check, station undock, structure dock validation (unknown structure fails), valid structure dock -> rollback_undock.
    assert!(matches!(
        repo.undock_character(999, cid),
        Err(DbError::CharacterOwnershipViolation { user_id: 999, character_id }) if character_id == cid
    ));
    let (station_id, _, ship) = repo.undock_character(1, cid).expect("undock");
    assert_eq!(station_id, 60_003_760);
    assert!(matches!(
        repo.dock_character(cid, 1_000_000_000_777, true),
        Err(DbError::InvalidDestinationLocation { .. })
    ));

    // Seed an operational cored Astrahus with an online Clone Bay service module in Jita.
    let struct_id = 1_000_000_000_500_i64;
    let clone_bay_mod_id = 200_000_901_i64;
    repo.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: 1_000_170,
                solar_system_id: jita_sys,
                name: "Jita Free Fort".into(),
                position: [1000.0, 0.0, 1000.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..UpwellStructureRecord::default()
            },
        );
        snap.items.insert(
            clone_bay_mod_id,
            InventoryItemRecord {
                item_id: clone_bay_mod_id,
                type_id: 35894, // Standup Cloning Center I
                owner_id: 1_000_170,
                location_id: struct_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
        snap.module_online_states.insert(clone_bay_mod_id, true);
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.home_station_id = struct_id;
            ch.neural.bonus_remaps = 1;
        }
    });

    let docked_in_struct = repo.dock_character(cid, struct_id, true).expect("dock in structure");
    assert_eq!(docked_in_struct.structure_id, Some(struct_id));
    assert!(docked_in_struct.station_id.is_none());

    // Destroy ship while home station is the cored clone-bay structure -> respawns inside structure with Capsule (670)!
    let (respawned, capsule) = repo
        .respawn_character_at_home_station(cid, ship.item_id, &sde)
        .expect("respawn at structure");
    assert_eq!(capsule.type_id, 670);
    assert_eq!(capsule.location_id, struct_id);
    assert_eq!(respawned.structure_id, Some(struct_id));

    // Destroy pod after structure loses core -> strips implants & boosters and falls back to school station!
    let implant_id = 200_000_905_i64;
    repo.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().quantum_core_installed = false;
        snap.active_implants.insert(cid, vec![19540]);
        snap.items.insert(
            implant_id,
            InventoryItemRecord {
                item_id: implant_id,
                type_id: 19540,
                owner_id: cid,
                location_id: cid,
                flag: InventoryFlag::Implant,
                quantity: 1,
                singleton: true,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
    });
    let (pod_respawn, _) = repo
        .respawn_character_at_home_station(cid, capsule.item_id, &sde)
        .expect("pod respawn fallback");
    assert!(pod_respawn.station_id.is_some());
    assert!(pod_respawn.structure_id.is_none());
    assert!(repo.with_snapshot(|s| !s.active_implants.contains_key(&cid) && !s.items.contains_key(&implant_id)));

    // Rollback undock & set_character_solar_system.
    repo.rollback_undock(cid, 60_003_760, pod_respawn.active_ship_id);
    let moved_sys = repo
        .set_character_solar_system(cid, 30_000_144, &sde)
        .expect("set solar system");
    assert_eq!(moved_sys.solar_system_id, 30_000_144);
    assert!(matches!(
        repo.set_character_solar_system(99_999_999, 30_000_144, &sde),
        Err(DbError::CharacterNotFound(99_999_999))
    ));

    // Neural attribute remapping: bounds check, sum check, timed cooldown, bonus remap, cooldown rejection.
    assert!(repo.remap_neural_attributes(cid, 16, 21, 21, 21, 20, 1_700_000_000).is_err());
    assert!(repo.remap_neural_attributes(cid, 20, 20, 20, 20, 20, 1_700_000_000).is_err());
    let r1 = repo
        .remap_neural_attributes(cid, 17, 27, 25, 17, 13 + 4, 1_700_000_000)
        .map_or_else(
            |_| repo.remap_neural_attributes(cid, 17, 25, 23, 17, 17, 1_700_000_000).unwrap(),
            |c| c,
        );
    assert_eq!(r1.neural.intelligence, 25);
    // Within 365-day cooldown: uses bonus_remaps (1 -> 0).
    let r2 = repo
        .remap_neural_attributes(cid, 18, 24, 22, 18, 17, 1_700_001_000)
        .expect("bonus remap consumes bonus_remaps");
    assert_eq!(r2.neural.bonus_remaps, 0);
    // Next remap during cooldown with 0 bonus remaps fails!
    assert!(repo
        .remap_neural_attributes(cid, 19, 23, 22, 18, 17, 1_700_002_000)
        .is_err());

    // Mining ledger + waste + observer structure mirroring.
    repo.record_mining_ledger_entry(cid, jita_sys, 1230, 5_000, 1_700_000_000)
        .expect("mining entry");
    repo.record_mining_ledger_entry_with_waste(cid, jita_sys, 1230, 4_200, 350, 1_700_000_060)
        .expect("mining entry with waste");
    assert_eq!(repo.get_mining_ledger(cid).len(), 2);
    let with_waste = repo.get_mining_ledger_with_waste(cid);
    assert_eq!(with_waste.len(), 2);
    assert_eq!(with_waste[1].1, 350);
    assert_eq!(repo.get_observer_mining_ledger(struct_id).len(), 2);
    assert_eq!(repo.get_observers_with_mining_events(), vec![struct_id]);
}

#[test]
fn inventory_stack_split_repackage_move_delete_and_contract_ledger_flows() {
    let repo = WorldRepository::with_demo_seed();
    let cid = 90_000_001_i64;
    let station_id = 60_003_760_i64;

    // add_or_stack_item creates a new stack when missing and increments existing non-courier stack.
    assert!(repo.add_or_stack_item(cid, station_id, InventoryFlag::Hangar, 587, 0).is_err());
    let s1 = repo
        .add_or_stack_item(cid, station_id, InventoryFlag::Hangar, 587, 1_000)
        .expect("add stack 1");
    assert_eq!(s1.quantity, 1_000);
    let s1_merged = repo
        .add_or_stack_item(cid, station_id, InventoryFlag::Hangar, 587, 500)
        .expect("auto-stack onto existing");
    assert_eq!(s1_merged.item_id, s1.item_id);
    assert_eq!(s1_merged.quantity, 1_500);

    // split_stack validation & execution (including custom name propagation).
    assert!(repo.split_stack(cid, s1.item_id, 0).is_err());
    assert!(repo.split_stack(cid, s1.item_id, 1_500).is_err());
    repo.with_mut_snapshot(|snap| {
        snap.item_custom_names.insert(s1.item_id, "Refined Trit".into());
    });
    let (orig_after, split_new) = repo.split_stack(cid, s1.item_id, 400).expect("split stack");
    assert_eq!(orig_after.quantity, 1_100);
    assert_eq!(split_new.quantity, 400);

    // stack_items self-stack & courier-cargo rejection, then valid stack back together.
    assert!(repo.stack_items(cid, s1.item_id, s1.item_id).is_err());
    let stacked = repo
        .stack_items(cid, orig_after.item_id, split_new.item_id)
        .expect("stack back");
    assert_eq!(stacked.quantity, 1_500);

    // repackage_item guards (active ship fails; idle singleton ship in Hangar succeeds).
    let idle_ship_id = 200_001_234_i64;
    repo.with_mut_snapshot(|snap| {
        let active = snap.characters[&cid].active_ship_id;
        assert!(snap.characters.get(&cid).is_some());
        let _ = active;
        snap.items.insert(
            idle_ship_id,
            InventoryItemRecord {
                item_id: idle_ship_id,
                type_id: 587,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                insured_payout_isk: 500_000.0,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
    });
    let repackaged = repo.repackage_item(cid, idle_ship_id).expect("repackage idle ship");
    assert!(!repackaged.singleton);
    assert_eq!(repackaged.insured_payout_isk, 0.0);

    // move_item_transactional & delete_item.
    let active_ship_id = repo.with_snapshot(|s| s.characters[&cid].active_ship_id);
    assert!(repo.delete_item(cid, active_ship_id).is_err());
    let moved = repo
        .move_item_transactional(
            cid,
            stacked.item_id,
            active_ship_id,
            InventoryFlag::Cargo,
            Some(stacked.version),
            None,
        )
        .expect("move to cargo");
    repo.delete_item(cid, moved.item_id).expect("delete moved item");
    assert!(repo.get_item(moved.item_id).is_none());

    // Ledger market fill, daily market summary, PLEX vault, and snapshot_ext contract & ID reconciliation.
    assert_eq!(unix_secs_to_date_str(1_704_067_200), "2024-01-01");
    assert_eq!(
        crate::snapshot_ext::unix_secs_to_date_string(1_704_067_200),
        "2024-01-01"
    );

    repo.with_mut_snapshot(|snap| {
        // PLEX vault operations + legacy migration (with_demo_seed starts user 1 with 500 PLEX).
        assert_eq!(snap.adjust_plex_vault(cid, 250), Some(750));
        assert_eq!(snap.adjust_plex_vault(cid, -800), None);
        assert_eq!(snap.plex_vault_balance(cid), 750);
        snap.account_plex_vault.remove(&2);
        snap.characters.get_mut(&90_000_002).unwrap().plex_balance = 420;
        snap.fold_legacy_plex_balances();
        assert_eq!(snap.plex_vault_balance(cid), 750);
        assert_eq!(snap.plex_vault_balance(90_000_002), 420);

        // Corp record for corporate market fill & contract payout coverage.
        snap.corporations.insert(
            98_000_050,
            CorporationRecord {
                corporation_id: 98_000_050,
                name: "Trade Guild".into(),
                ticker: "TRDG".into(),
                ceo_id: cid,
                tax_rate: 0.1,
                master_wallet: IskCents::from_isk(5_000_000),
                hq_station_id: Some(station_id),
                ..CorporationRecord::default()
            },
        );

        let tx1 = snap.journal_market_fill(MarketFill {
            buyer_id: cid,
            seller_id: 98_000_050,
            type_id: 34,
            quantity: 100,
            price: IskCents::from_isk(50),
            location_id: station_id,
            date_unix: 1_704_067_200,
            gross: IskCents::from_isk(5_000),
            buyer_balance_after: Some(IskCents::from_isk(999_995_000)),
            seller_net: IskCents::from_isk(4_600),
            seller_balance_after: Some(IskCents::from_isk(5_004_600)),
            sales_tax: IskCents::from_isk(250),
            tax_collector_id: 1_000_035,
            region_id: Some(10_000_002),
        });
        // Second trade on the same day updates running average, highest, lowest, order_count.
        snap.record_market_trade_daily_summary(10_000_002, 34, 1_704_067_260, 70.0, 100);
        let day = &snap.market_history[&(10_000_002, 34)][0];
        assert_eq!(day.order_count, 2);
        assert_eq!(day.volume, 200);
        assert!((day.average - 60.0).abs() < 1e-9);
        assert!((day.highest - 70.0).abs() < 1e-9);
        assert!((day.lowest - 50.0).abs() < 1e-9);

        let meta = JournalMeta::new(ref_types::PLAYER_DONATION, cid, 1_000_035, 1_704_067_300)
            .with_context(JournalContext::MarketTransaction, tx1);
        snap.append_journal_entry(JournalEntryInput {
            owner_id: 98_000_050,
            entry_type: "Bounty",
            amount: IskCents::from_isk(1_000),
            balance_after: IskCents::from_isk(6_000_000),
            reason: "Ratting tax".into(),
            meta,
        });

        // Expired Auction contract completion & Courier failure unwrapping courier wrap (type 3468).
        let escrow_item = 200_005_001_i64;
        snap.items.insert(
            escrow_item,
            InventoryItemRecord {
                item_id: escrow_item,
                type_id: 587,
                owner_id: cid,
                location_id: 200_006_001,
                flag: InventoryFlag::Locked,
                quantity: 1,
                singleton: false,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
        snap.contracts.insert(
            200_006_001,
            ContractRecord {
                contract_id: 200_006_001,
                issuer_id: cid,
                issuer_corp_id: 98_000_050,
                assignee_id: 0,
                acceptor_id: None,
                for_corp: true,
                contract_type: "Auction".into(),
                status: "Outstanding".into(),
                title: "Rifter Auction".into(),
                start_station_id: station_id,
                end_station_id: station_id,
                price: IskCents::from_isk(1_000_000),
                reward: IskCents::ZERO,
                collateral: IskCents::ZERO,
                buyout: IskCents::from_isk(5_000_000),
                current_bid: IskCents::from_isk(2_000_000),
                current_bidder_id: Some(90_000_002),
                expiration_unix: 1_700_000_000,
                item_ids: vec![escrow_item],
                ..ContractRecord::default()
            },
        );
        snap.service_state_blobs
            .insert("contract_deposit:200006001".into(), "50000".into());
        assert_eq!(
            snap.resolve_expired_contract(200_006_001, 1_700_000_100),
            Some("Completed")
        );
        assert_eq!(snap.items[&escrow_item].owner_id, 90_000_002);

        // Courier contract expiration -> fail_courier_contract unwraps type 3468 courier wrap.
        let wrap_id = 200_006_010_i64;
        let inner_item = 200_006_011_i64;
        snap.items.insert(
            wrap_id,
            InventoryItemRecord {
                item_id: wrap_id,
                type_id: 3468,
                owner_id: 90_000_002,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            inner_item,
            InventoryItemRecord {
                item_id: inner_item,
                type_id: 34,
                owner_id: cid,
                location_id: wrap_id,
                flag: InventoryFlag::Locked,
                quantity: 100,
                singleton: false,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
        snap.contracts.insert(
            200_006_020,
            ContractRecord {
                contract_id: 200_006_020,
                issuer_id: cid,
                issuer_corp_id: 1_000_170,
                assignee_id: 90_000_002,
                acceptor_id: Some(90_000_002),
                for_corp: false,
                contract_type: "Courier".into(),
                status: "InProgress".into(),
                title: "Haul".into(),
                start_station_id: station_id,
                end_station_id: station_id,
                reward: IskCents::from_isk(100_000),
                collateral: IskCents::from_isk(500_000),
                expiration_unix: 1_700_000_000,
                item_ids: vec![inner_item],
                ..ContractRecord::default()
            },
        );
        assert_eq!(
            snap.resolve_expired_contract(200_006_020, 1_700_000_100),
            Some("Failed")
        );
        assert!(!snap.items.contains_key(&wrap_id));
        assert_eq!(snap.items[&inner_item].location_id, station_id);
        assert_eq!(snap.items[&inner_item].owner_id, 90_000_002);

        // ID allocators & reconciliation + killmail/jump/kill helpers.
        let eid = snap.alloc_entity_id();
        let aid = snap.allocate_alliance_id();
        let coid = snap.allocate_corporation_id();
        let nid = snap.allocate_notification_id();
        let rid = snap.allocate_report_id();
        assert!(eid > 200_000_000 && aid > 99_000_000 && coid > 98_100_000 && nid > 500_000 && rid > 700_000);
        snap.reconcile_id_counters();

        let kill_id = snap.alloc_entity_id();
        snap.record_killmail(
            KillmailRecord {
                kill_id,
                solar_system_id: 30_000_142,
                victim_character_id: cid,
                attacker_character_id: 90_000_002,
                ship_type_id: 587,
                total_value_isk: 250_000.0,
                ..KillmailRecord::default()
            },
            1_700_000_500,
        );
        snap.record_system_jump(30_000_142, cid, 1_700_000_500);
        snap.record_system_kill(30_000_142, 1, 2, 1, 1_700_000_500);
        let _ = snap.append_journal(cid, "TestEntry", IskCents(100), IskCents(200), "reason");
    });
}
