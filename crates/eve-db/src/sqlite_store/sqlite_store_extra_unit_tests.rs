//! Comprehensive unit and mutation-killing tests for `sqlite_store` submodules:
//! `fingerprints`, `industry_market_pve_store`, `ledger_store`, `notes_lp_sequence_store`,
//! `social_store`, and `sync_ext`.

use super::*;
use crate::durable_domain_types::SocialContactNotificationRecord;
use crate::ledger::{JournalContext, JournalMeta, MarketTransactionRecord};
use crate::persistence::WorldRepository;
use crate::snapshot_ext::*;
use crate::types::*;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::hash::Hasher;

#[test]
fn fnv_hasher_and_row_fingerprints_detect_every_field_mutation() {
    let mut h1 = fnv1a_hasher();
    h1.write_u8(7);
    h1.write_bool(true);
    h1.write_u32(123);
    h1.write_i32(-456);
    h1.write_u64(789);
    h1.write_i64(-1011);
    h1.write_f64(3.5);
    h1.write_str("abc");
    Hasher::write(&mut h1, b"xyz");
    let f1 = Hasher::finish(&h1);
    assert_eq!(f1, h1.finish());

    let base_acc = AccountRecord {
        user_id: 10,
        username: "pilot".into(),
        password_token: "tok".into(),
        role: 3,
        banned_until_unix: None,
        ban_reason: None,
    };
    let acc_fp = fp_account(&base_acc);
    assert_ne!(fp_account(&AccountRecord { user_id: 11, ..base_acc.clone() }), acc_fp);
    assert_ne!(fp_account(&AccountRecord { username: "pilot2".into(), ..base_acc.clone() }), acc_fp);
    assert_ne!(fp_account(&AccountRecord { password_token: "tok2".into(), ..base_acc.clone() }), acc_fp);
    assert_ne!(fp_account(&AccountRecord { role: 4, ..base_acc.clone() }), acc_fp);
    assert_ne!(fp_account(&AccountRecord { banned_until_unix: Some(100), ..base_acc.clone() }), acc_fp);
    assert_ne!(fp_account(&AccountRecord { ban_reason: Some("cheat".into()), ..base_acc.clone() }), acc_fp);

    let demo = WorldRepository::create_demo_snapshot();
    let base_char = demo.characters[&90_000_001].clone();
    let char_fp = fp_character(&base_char);
    let check_char = |mutator: &dyn Fn(&mut CharacterRecord), label: &str| {
        let mut c = base_char.clone();
        mutator(&mut c);
        assert_ne!(fp_character(&c), char_fp, "fp_character missed {label}");
    };
    check_char(&|c| c.character_id += 1, "character_id");
    check_char(&|c| c.user_id += 1, "user_id");
    check_char(&|c| c.name.push('X'), "name");
    check_char(&|c| c.race_id += 1, "race_id");
    check_char(&|c| c.bloodline_id += 1, "bloodline_id");
    check_char(&|c| c.ancestry_id += 1, "ancestry_id");
    check_char(&|c| c.school_id += 1, "school_id");
    check_char(&|c| c.corporation_id += 1, "corporation_id");
    check_char(&|c| c.alliance_id = Some(99_000_001), "alliance_id");
    check_char(&|c| c.war_faction_id = Some(500_001), "war_faction_id");
    check_char(&|c| c.station_id = None, "station_id");
    check_char(&|c| c.structure_id = Some(1_000_000_000_001), "structure_id");
    check_char(&|c| c.solar_system_id += 1, "solar_system_id");
    check_char(&|c| c.constellation_id += 1, "constellation_id");
    check_char(&|c| c.region_id += 1, "region_id");
    check_char(&|c| c.active_ship_id += 1, "active_ship_id");
    check_char(&|c| c.balance.0 += 1, "balance");
    check_char(&|c| c.plex_balance += 1, "plex_balance");
    check_char(&|c| c.security_status += 0.1, "security_status");
    check_char(&|c| c.clone_grade = 1, "clone_grade");
    check_char(&|c| c.home_station_id += 1, "home_station_id");
    check_char(&|c| c.neural.charisma += 1, "neural.charisma");
    check_char(&|c| c.neural.intelligence += 1, "neural.intelligence");
    check_char(&|c| c.neural.memory += 1, "neural.memory");
    check_char(&|c| c.neural.perception += 1, "neural.perception");
    check_char(&|c| c.neural.willpower += 1, "neural.willpower");
    check_char(&|c| c.neural.bonus_remaps += 1, "neural.bonus_remaps");
    check_char(&|c| c.neural.last_remap_unix = Some(50), "neural.last_remap_unix");
    check_char(&|c| c.neural.remap_cooldown_until_unix = Some(60), "neural.remap_cooldown_until_unix");
    check_char(&|c| c.skill_points += 1.0, "skill_points");
    check_char(&|c| c.free_skill_points += 1.0, "free_skill_points");
    check_char(&|c| c.last_clone_jump_unix += 1, "last_clone_jump_unix");
    check_char(&|c| c.jump_fatigue_minutes += 1.0, "jump_fatigue_minutes");
    check_char(&|c| c.jump_cooldown_until_unix += 1, "jump_cooldown_until_unix");
    check_char(&|c| c.jump_fatigue_until_unix += 1, "jump_fatigue_until_unix");
    check_char(&|c| c.fleet_id = Some(400), "fleet_id");
    check_char(&|c| c.version += 1, "version");
    check_char(&|c| c.system_epoch += 1, "system_epoch");
    check_char(&|c| c.activity_status = Some("busy".into()), "activity_status");
    check_char(&|c| { c.loyalty_points.insert(1_000_125, 99); }, "loyalty_points");
    check_char(&|c| { c.standings.insert(1_000_125, -2.5); }, "standings");

    let base_item = demo.items[&100_000_001].clone();
    let item_fp = fp_item(&base_item);
    let check_item = |mutator: &dyn Fn(&mut InventoryItemRecord), label: &str| {
        let mut i = base_item.clone();
        mutator(&mut i);
        assert_ne!(fp_item(&i), item_fp, "fp_item missed {label}");
    };
    check_item(&|i| i.item_id += 1, "item_id");
    check_item(&|i| i.type_id += 1, "type_id");
    check_item(&|i| i.owner_id += 1, "owner_id");
    check_item(&|i| i.location_id += 1, "location_id");
    check_item(&|i| i.flag = InventoryFlag::Cargo, "flag");
    check_item(&|i| i.quantity += 1, "quantity");
    check_item(&|i| i.singleton = !i.singleton, "singleton");
    check_item(&|i| i.version += 1, "version");
    check_item(&|i| i.source_mutaplasmid_id = Some(47_740), "source_mutaplasmid_id");
    check_item(&|i| i.insured_payout_isk = 1_500.0, "insured_payout_isk");
    check_item(&|i| i.is_blueprint_copy = true, "is_blueprint_copy");
    check_item(&|i| i.blueprint_runs_remaining = Some(10), "blueprint_runs_remaining");
    check_item(&|i| { i.dynamic_attributes.insert(64, 1.25); }, "dynamic_attributes");
}

#[test]
fn fp_market_contract_system_structure_and_slices_detect_mutations() {
    let base_ord = MarketOrderRecord {
        order_id: 700_000_001, owner_id: 90_000_001, station_id: 60_003_760, region_id: 10_000_002,
        type_id: 34, is_bid: false, price: IskCents(500), vol_entered: 100, vol_remaining: 80,
        min_volume: 1, is_npc: false, escrow: IskCents(1000),
    };
    let ord_fp = fp_market_order(&base_ord);
    assert_ne!(fp_market_order(&MarketOrderRecord { order_id: 2, ..base_ord.clone() }), ord_fp);
    assert_ne!(fp_market_order(&MarketOrderRecord { owner_id: 2, ..base_ord.clone() }), ord_fp);
    assert_ne!(fp_market_order(&MarketOrderRecord { station_id: 2, ..base_ord.clone() }), ord_fp);
    assert_ne!(fp_market_order(&MarketOrderRecord { region_id: 2, ..base_ord.clone() }), ord_fp);
    assert_ne!(fp_market_order(&MarketOrderRecord { type_id: 35, ..base_ord.clone() }), ord_fp);
    assert_ne!(fp_market_order(&MarketOrderRecord { is_bid: true, ..base_ord.clone() }), ord_fp);
    assert_ne!(fp_market_order(&MarketOrderRecord { price: IskCents(600), ..base_ord.clone() }), ord_fp);
    assert_ne!(fp_market_order(&MarketOrderRecord { vol_entered: 101, ..base_ord.clone() }), ord_fp);
    assert_ne!(fp_market_order(&MarketOrderRecord { vol_remaining: 79, ..base_ord.clone() }), ord_fp);
    assert_ne!(fp_market_order(&MarketOrderRecord { min_volume: 2, ..base_ord.clone() }), ord_fp);
    assert_ne!(fp_market_order(&MarketOrderRecord { is_npc: true, ..base_ord.clone() }), ord_fp);
    assert_ne!(fp_market_order(&MarketOrderRecord { escrow: IskCents(1001), ..base_ord.clone() }), ord_fp);

    let mut orders = crate::CowMap::new();
    let d0 = fp_market_orders_digest(&orders);
    orders.insert(1, base_ord.clone());
    assert_ne!(fp_market_orders_digest(&orders), d0);

    let base_con = ContractRecord {
        contract_id: 1, issuer_id: 90_000_001, issuer_corp_id: 98_000_001, assignee_id: 90_000_002,
        acceptor_id: Some(90_000_002), for_corp: false, availability: "Public".into(),
        contract_type: "Auction".into(), status: "Outstanding".into(), title: "Sale".into(),
        start_station_id: 60_003_760, end_station_id: 60_003_760, price: IskCents(100),
        reward: IskCents(200), collateral: IskCents(300), buyout: IskCents(400),
        current_bid: IskCents(150), current_bidder_id: Some(90_000_002), date_issued_unix: 1000,
        expiration_unix: 2000, num_days: 3, item_ids: vec![100_000_003],
        requested_items: vec![(34, 50)], bid_history: vec![(90_000_002, IskCents(150), 1500)],
    };
    let con_fp = fp_contract(&base_con);
    assert_ne!(fp_contract(&ContractRecord { acceptor_id: None, ..base_con.clone() }), con_fp);
    assert_ne!(fp_contract(&ContractRecord { current_bidder_id: None, ..base_con.clone() }), con_fp);
    assert_ne!(fp_contract(&ContractRecord { item_ids: vec![100_000_004], ..base_con.clone() }), con_fp);
    assert_ne!(fp_contract(&ContractRecord { requested_items: vec![(34, 51)], ..base_con.clone() }), con_fp);
    assert_ne!(fp_contract(&ContractRecord { bid_history: vec![], ..base_con.clone() }), con_fp);

    let mut base_sys = SystemDynamicState {
        solar_system_id: 30_000_142, dbs_multiplier: 1.2, ess_main_bank: IskCents(500),
        ess_reserve_bank: IskCents(600), ess_linked_character_id: Some(90_000_001),
        ess_link_started_tick: Some(10), ess_cycle_started_unix: 999,
        ess_main_bank_contributors: HashMap::from([(90_000_001, 250), (90_000_002, 250)]),
        sov_alliance_id: Some(99_000_001), military_index: 1, industrial_index: 2, strategic_index: 3,
        fw_contested_pct: 25.0, insurgency_corruption_stage: 2, insurgency_suppression_stage: 1,
        wormhole_remaining_mass_kg: 1_000_000.0, wormhole_destination_system_id: Some(31_000_005),
        signature_position: Some([1.0, 2.0, 3.0]), fw_complex_position: Some([4.0, 5.0, 6.0]),
        fw_capture_seconds_elapsed: 30, fw_capture_duration_seconds: 600,
        last_fw_capture_tick: 12, last_sov_claim_tick: 15,
    };
    let sys_fp = fp_system_state(&base_sys);
    base_sys.signature_position = None;
    assert_ne!(fp_system_state(&base_sys), sys_fp);
    let sys_fp2 = fp_system_state(&base_sys);
    base_sys.fw_complex_position = None;
    assert_ne!(fp_system_state(&base_sys), sys_fp2);
    let sys_fp3 = fp_system_state(&base_sys);
    base_sys.ess_main_bank_contributors.insert(90_000_001, 300);
    assert_ne!(fp_system_state(&base_sys), sys_fp3);

    let mut st = UpwellStructureRecord {
        structure_id: 1_000_000_000_001, type_id: 35832, owner_corp_id: 98_000_001,
        solar_system_id: 30_000_142, name: "Astrahus".into(), position: [10.0, 20.0, 30.0],
        state: "ShieldVulnerable".into(), quantum_core_installed: true, fuel_blocks: 100,
        shield_hp: 5000.0, armor_hp: 5000.0, hull_hp: 5000.0, reinforced_until_unix: 0,
        last_damage_tick: 5, last_damage_unix: 100, self_destruct_at_unix: Some(5000),
    };
    let st_fp = fp_structure(&st);
    st.self_destruct_at_unix = None;
    assert_ne!(fp_structure(&st), st_fp);

    let wj = vec![WalletJournalEntry {
        entry_id: 1, character_id: 90_000_001, entry_type: "Bounty".into(),
        amount: IskCents(100), balance_after: IskCents(1100), reason: "pirate".into(),
    }];
    assert_ne!(fp_wallet_journal_slice(&wj), fp_wallet_journal_slice(&[]));
    let mails = vec![MailRecord {
        message_id: 1, sender_id: 90_000_001, recipient_id: 90_000_002,
        subject: "Hi".into(), body: "Hello".into(),
    }];
    assert_ne!(fp_mails_slice(&mails), fp_mails_slice(&[]));
    let sq = vec![SkillQueueEntry {
        type_id: 3300, target_level: 5, start_sp: 45_255.0, end_sp: 256_000.0,
        duration_seconds: 3600.0, started_at_unix: Some(1_700_000_000),
    }];
    assert_ne!(fp_skill_queue(&sq), fp_skill_queue(&[]));

    let mut sp6 = Sp6PersistedState::default();
    let sp6_0 = fp_sp6_lifecycle(&sp6);
    sp6.mission_initial_npc_counts.insert("90000001:1".into(), 5);
    let sp6_1 = fp_sp6_lifecycle(&sp6);
    assert_ne!(sp6_0, sp6_1);
    sp6.mission_initial_npc_types.insert("90000001:1".into(), vec![1001, 1002]);
    assert_ne!(fp_sp6_lifecycle(&sp6), sp6_1);
}

#[test]
fn ledger_store_sync_load_and_selective_rollback() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    let mut snap = WorldStateSnapshot::default();
    snap.wallet_journal_meta.insert(
        10,
        JournalMeta::new(2, 90_000_001, 90_000_002, 1_700_000_000)
            .with_context(JournalContext::MarketTransaction, 500),
    );
    snap.market_transactions.insert(
        500,
        MarketTransactionRecord {
            transaction_id: 500, date_unix: 1_700_000_000, type_id: 34, quantity: 100,
            price: IskCents(500), location_id: 60_003_760, buyer_id: 90_000_001,
            seller_id: 90_000_002, buyer_account_key: 1000, seller_account_key: 1000, journal_ref_id: 10,
        },
    );
    snap.account_plex_vault.insert(1, 250);

    {
        let tx = conn.transaction().unwrap();
        assert!(domain_rows::sync_ledger_domain_rows(&tx, &snap).unwrap());
        tx.commit().unwrap();
    }
    {
        let tx = conn.transaction().unwrap();
        assert!(!domain_rows::sync_ledger_domain_rows(&tx, &snap).unwrap());
        tx.commit().unwrap();
    }

    let mut loaded = WorldStateSnapshot::default();
    domain_rows::load_ledger_domain_rows(
        &conn,
        &mut loaded,
        &mut quarantine::LoadContext::quarantine_in_memory(),
    )
    .unwrap();
    assert_eq!(loaded.wallet_journal_meta, snap.wallet_journal_meta);
    assert_eq!(loaded.market_transactions, snap.market_transactions);
    assert_eq!(loaded.account_plex_vault, snap.account_plex_vault);

    snap.wallet_journal_meta.clear();
    snap.market_transactions.clear();
    snap.account_plex_vault.clear();
    {
        let tx = conn.transaction().unwrap();
        assert!(domain_rows::sync_ledger_domain_rows(&tx, &snap).unwrap());
        tx.commit().unwrap();
    }
    let mut loaded_empty = WorldStateSnapshot::default();
    domain_rows::load_ledger_domain_rows(
        &conn,
        &mut loaded_empty,
        &mut quarantine::LoadContext::quarantine_in_memory(),
    )
    .unwrap();
    assert!(loaded_empty.wallet_journal_meta.is_empty());
    assert!(loaded_empty.market_transactions.is_empty());
    assert!(loaded_empty.account_plex_vault.is_empty());
}

#[test]
fn notes_lp_sequence_store_sync_load_and_selective_rollback() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    let mut snap = WorldStateSnapshot::default();
    snap.next_alliance_id = 99_000_111;
    snap.next_corporation_id = 98_000_222;
    snap.next_notification_id = 940_000_333;
    snap.next_report_id = 700_444;
    snap.owner_notes.insert(
        1,
        OwnerNoteRecord {
            note_id: 1, character_id: 90_000_001, item_id: 60_003_760,
            label: "Note".into(), note: "Hello".into(), timestamp_unix: 1_700_000_000,
        },
    );
    snap.corp_lp_balances.insert((98_000_001, 1_000_035), 4_500);
    snap.char_lp_logs.insert(
        90_000_001,
        vec![LpRewardLogEntry {
            log_id: 1, corp_id: 1_000_035, amount: 500,
            reason: "mission".into(), timestamp_unix: 1_700_000_000,
        }],
    );
    snap.sp6_lifecycle.last_downtime_day_idx = Some(19_900);

    let mut fp = TableFingerprints::default();
    {
        let tx = conn.transaction().unwrap();
        assert!(notes_lp_sequence_store::sync_d6(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    {
        let tx = conn.transaction().unwrap();
        assert!(!notes_lp_sequence_store::sync_d6(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }

    notes_lp_sequence_store::rollback_d6(&conn, &mut snap, &fp);
    assert_eq!(snap.next_alliance_id, 99_000_111);

    snap.next_alliance_id = 1;
    snap.owner_notes.clear();
    snap.corp_lp_balances.clear();
    snap.char_lp_logs.clear();
    snap.sp6_lifecycle.last_downtime_day_idx = None;
    notes_lp_sequence_store::rollback_d6(&conn, &mut snap, &fp);
    assert_eq!(snap.next_alliance_id, 99_000_111);
    assert_eq!(snap.owner_notes.len(), 1);
    assert_eq!(snap.corp_lp_balances.get(&(98_000_001, 1_000_035)), Some(&4_500));
    assert_eq!(snap.char_lp_logs.len(), 1);
    assert_eq!(snap.sp6_lifecycle.last_downtime_day_idx, Some(19_900));

    snap.owner_notes.clear();
    snap.corp_lp_balances.clear();
    snap.char_lp_logs.clear();
    {
        let tx = conn.transaction().unwrap();
        assert!(notes_lp_sequence_store::sync_d6(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    let mut loaded = WorldStateSnapshot::default();
    notes_lp_sequence_store::load_d6(&conn, &mut loaded);
    assert!(loaded.owner_notes.is_empty());
    assert!(loaded.corp_lp_balances.is_empty());
    assert!(loaded.char_lp_logs.is_empty());
}

#[test]
fn industry_market_pve_store_sync_load_and_rollback_all_blobs() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    let mut snap = WorldStateSnapshot::default();
    snap.module_online_states.insert(100_000_005, true);
    snap.module_active_states.insert(100_000_005);
    snap.module_active_charges.insert(100_000_005, 190);
    snap.weapon_banks.entry(100_000_001).or_default().insert(100_000_005, vec![100_000_006]);
    snap.paused_skill_queues.insert(90_000_001);
    snap.quasar_state_blobs.insert("k1".into(), "v1".into());
    snap.service_state_blobs.insert("s1".into(), "v2".into());
    snap.mining_ledger.push(MiningLedgerRecord {
        character_id: 90_000_001, solar_system_id: 30_000_142, type_id: 1230,
        quantity: 500, date: "2025-01-01".into(), timestamp_unix: 1_735_689_600,
    });

    let mut fp = TableFingerprints::default();
    {
        let tx = conn.transaction().unwrap();
        assert!(industry_market_pve_store::sync_d6_batch4(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    {
        let tx = conn.transaction().unwrap();
        assert!(!industry_market_pve_store::sync_d6_batch4(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }

    industry_market_pve_store::rollback_d6_batch4(&conn, &mut snap, &fp);
    assert_eq!(snap.mining_ledger.len(), 1);

    snap.module_online_states.clear();
    snap.module_active_states.clear();
    snap.weapon_banks.clear();
    snap.paused_skill_queues.clear();
    snap.mining_ledger.clear();
    industry_market_pve_store::rollback_d6_batch4(&conn, &mut snap, &fp);
    assert_eq!(snap.module_online_states.get(&100_000_005), Some(&true));
    assert!(snap.module_active_states.contains(&100_000_005));
    assert_eq!(snap.weapon_banks[&100_000_001][&100_000_005], vec![100_000_006]);
    assert!(snap.paused_skill_queues.contains(&90_000_001));
    assert_eq!(snap.mining_ledger.len(), 1);
    assert_eq!(snap.mining_ledger[0].quantity, 500);
}

#[test]
fn social_store_and_sync_ext_cover_all_collections_and_deletions() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    let mut snap = WorldStateSnapshot::default();
    snap.read_mails.insert((90_000_001, 1));
    snap.deleted_mails.insert((90_000_001, 2));
    snap.trashed_mails.insert((90_000_001, 3));
    snap.mail_labels.insert(90_000_001, vec![MailLabelRecord { label_id: 1, name: "Important".into(), color: "#ffffff".into() }]);
    snap.mail_assigned_labels.insert((90_000_001, 1), vec![1]);
    snap.mailing_lists.insert(
        10,
        MailingListRecord {
            list_id: 10, name: "Fleet".into(), owner_id: 90_000_001, default_access: 1,
            default_role: 0, cost: 0, members: HashMap::from([(90_000_001, 2)]),
            entity_access: HashMap::new(), welcome_mail: Some(("Sub".into(), "Body".into())),
            mail_ids: HashSet::from([1]),
        },
    );
    snap.calendar_events.insert(
        20,
        CalendarEventRecord {
            event_id: 20, owner_id: 90_000_001, creator_id: 90_000_001,
            event_date_time: 1_700_000_000, duration: 60, importance: 1,
            title: "Op".into(), description: "Formup".into(), scope: "Personal".into(),
            responses: HashMap::new(), is_deleted: false,
        },
    );
    snap.bookmarks.insert(
        30,
        BookmarkRecord {
            bookmark_id: 30, owner_id: 90_000_001, label: "Spot".into(),
            solar_system_id: 30_000_142, x: 1.0, y: 2.0, z: 3.0, item_id: None,
            memo: String::new(), folder_id: Some(40), is_shared: true,
        },
    );
    snap.bookmark_subfolders.insert(30, 41);
    snap.bookmark_expiries.insert(30, 1_800_000_000);
    snap.bookmark_folders.insert(
        40,
        BookmarkFolderRecord {
            folder_id: 40, owner_id: 90_000_001, folder_name: "Shared".into(),
            is_shared: true, ..Default::default()
        },
    );
    snap.bookmark_folder_acls.insert(40, vec![50]);
    snap.bookmark_subfolder_parents.insert(40, 39);
    snap.bookmark_folder_known_by.insert((40, 90_000_001), true);
    snap.access_groups.insert(50, vec![90_000_001, 90_000_002]);
    snap.chat_channels.insert(
        "player_1".into(),
        ChatChannelRecord {
            channel_id: "player_1".into(), name: "Lounge".into(), motd: "Hi".into(),
            owner_id: Some(90_000_001), members: HashSet::from([90_000_001]),
            is_system_local: false, solar_system_id: None, is_private: true,
            operators: HashSet::new(), password: None, is_delayed_local: false,
        },
    );
    snap.chat_channel_acl.insert(
        "player_1".into(),
        ChatChannelAclRecord {
            allow_list: HashSet::from([90_000_002]), deny_list: HashSet::new(), muted_until: HashMap::new(),
        },
    );
    let contact = SocialContactRecord {
        contact_id: 90_000_002, contact_type: "character".into(), standing: 5.0,
        is_watched: true, is_blocked: false, label_ids: vec![1],
    };
    snap.char_contacts.insert(90_000_001, vec![contact.clone()]);
    snap.char_contact_notifications.insert(
        90_000_001,
        vec![SocialContactNotificationRecord {
            notification_id: 1, sender_character_id: 90_000_002,
            send_date_unix: 1_700_000_000, standing_level: 5.0, message: "Added".into(),
        }],
    );
    snap.char_contact_labels.insert(90_000_001, vec![(1, "Friends".into())]);
    snap.corp_contacts.insert(98_000_001, vec![contact.clone()]);
    snap.corp_contact_labels.insert(98_000_001, vec![(1, "Blues".into(), 2)]);
    snap.alliance_contacts.insert(99_000_001, vec![contact]);
    snap.alliance_contact_labels.insert(99_000_001, vec![(1, "Allies".into())]);
    snap.char_notes.insert((90_000_001, 90_000_002), "Trusted".into());

    snap.character_last_jump_unix.insert(90_000_001, 1_700_000_100);
    snap.character_cspa_charges.insert(90_000_001, IskCents(2_950_00));
    snap.mail_timestamps.insert(1, 1_700_000_200);
    snap.character_login_stats.insert(
        90_000_001,
        CharacterLoginStats { last_login_unix: 1_700_000_300, last_logout_unix: 1_700_000_400, logins_count: 7 },
    );
    snap.item_custom_names.insert(100_000_001, "My Rifter".into());
    snap.item_positions.insert(100_000_001, [10.0, 20.0, 30.0]);
    snap.structure_vulnerability_windows.insert(1_000_000_000_001, (1000, 2000));
    snap.skill_history.insert(
        90_000_001,
        vec![SkillHistoryEntry {
            log_date_ft: 133_485_408_000_000_000, event_type_id: 33,
            skill_type_id: 3300, skill_level: 4, absolute_points: 45_255.0,
        }],
    );
    snap.character_doll_info.insert(90_000_001, ("dna_1".into(), "meta_1".into()));

    let mut fp = TableFingerprints::default();
    {
        let tx = conn.transaction().unwrap();
        assert!(social_store::sync_d6_social(&tx, &snap, &mut fp).unwrap());
        assert!(sync_ext::sync_extension_tables(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    {
        let tx = conn.transaction().unwrap();
        assert!(!social_store::sync_d6_social(&tx, &snap, &mut fp).unwrap());
        assert!(!sync_ext::sync_extension_tables(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }

    snap.char_notes.clear();
    social_store::load_d6_social_into_snapshot(&conn, &mut snap).unwrap();
    assert_eq!(snap.char_notes.get(&(90_000_001, 90_000_002)), Some(&"Trusted".to_string()));

    snap.character_last_jump_unix.clear();
    snap.character_cspa_charges.clear();
    snap.mail_timestamps.clear();
    snap.character_login_stats.clear();
    snap.item_custom_names.clear();
    snap.item_positions.clear();
    snap.structure_vulnerability_windows.clear();
    snap.skill_history.clear();
    snap.character_doll_info.clear();
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_ext::sync_extension_tables(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    assert!(fp.character_last_jump_unix.is_empty());
    assert!(fp.character_cspa_charges.is_empty());
    assert!(fp.mail_timestamps.is_empty());
    assert!(fp.character_login_stats.is_empty());
    assert!(fp.item_custom_names.is_empty());
    assert!(fp.item_positions.is_empty());
    assert!(fp.structure_vulnerability_windows.is_empty());
    assert!(fp.skill_history.is_empty());
    assert!(fp.character_doll_info.is_empty());
}

#[test]
fn ws_a3_fingerprints_fnv_serializer_all_primitives_and_order_independence() {
    use serde::{ser::Error as _, Serialize, Serializer};

    // FnvSerError trait impls: Display, Debug, Error, serde::ser::Error::custom
    let err = fingerprints::FnvSerError::custom("bad");
    assert_eq!(err.to_string(), "fnv serialization error");
    assert!(!format!("{err:?}").is_empty());
    let _dyn_err: &dyn std::error::Error = &err;

    // Raw bytes wrapper triggering FnvSerializer::serialize_bytes (tag 6)
    struct BytesWrap<'a>(&'a [u8]);
    impl Serialize for BytesWrap<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_bytes(self.0)
        }
    }

    // Order-variant map wrapper exercising SerializeMap::serialize_key / serialize_value directly
    struct KVSlice<'a>(&'a [(&'static str, i64)]);
    impl Serialize for KVSlice<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeMap;
            let mut m = serializer.serialize_map(Some(self.0.len()))?;
            for &(k, v) in self.0 {
                m.serialize_key(k)?;
                m.serialize_value(&v)?;
            }
            m.end()
        }
    }

    // Open-length seq wrapper exercising serialize_seq(None)
    struct UnboundedSeq<'a>(&'a [u8]);
    impl Serialize for UnboundedSeq<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeSeq;
            let mut seq = serializer.serialize_seq(None)?;
            for b in self.0 {
                seq.serialize_element(b)?;
            }
            seq.end()
        }
    }

    #[derive(Serialize)]
    struct UnitStruct;
    #[derive(Serialize)]
    struct NewtypeStruct(u16);
    #[derive(Serialize)]
    struct TupleStruct(i8, i16);
    #[derive(Serialize)]
    enum AllVariants {
        UnitVar,
        NewtypeVar(u32),
        TupleVar(f32, char),
        StructVar { flag: bool, small: u8 },
    }
    #[derive(Serialize)]
    struct FullShowcase<'a> {
        b: bool,
        i8v: i8,
        i16v: i16,
        i32v: i32,
        i64v: i64,
        u8v: u8,
        u16v: u16,
        u32v: u32,
        u64v: u64,
        f32v: f32,
        neg_zero_f64: f64,
        pos_f64: f64,
        ch: char,
        s: &'a str,
        raw: BytesWrap<'a>,
        none_v: Option<u32>,
        some_v: Option<u32>,
        unit_v: (),
        unit_s: UnitStruct,
        newtype_s: NewtypeStruct,
        tup_s: TupleStruct,
        tup: (i32, &'static str),
        e1: AllVariants,
        e2: AllVariants,
        e3: AllVariants,
        e4: AllVariants,
        seq_unbounded: UnboundedSeq<'a>,
    }

    let make = |neg_zero: f64| FullShowcase {
        b: true,
        i8v: -7,
        i16v: -300,
        i32v: -70_000,
        i64v: -9_000_000,
        u8v: 250,
        u16v: 65_000,
        u32v: 4_000_000,
        u64v: 99_000_000_000,
        f32v: 1.25,
        neg_zero_f64: neg_zero,
        pos_f64: -0.0,
        ch: 'λ',
        s: "hello",
        raw: BytesWrap(b"\x01\x02\x03"),
        none_v: None,
        some_v: Some(42),
        unit_v: (),
        unit_s: UnitStruct,
        newtype_s: NewtypeStruct(9),
        tup_s: TupleStruct(-1, 2),
        tup: (3, "tup"),
        e1: AllVariants::UnitVar,
        e2: AllVariants::NewtypeVar(77),
        e3: AllVariants::TupleVar(2.5, 'Z'),
        e4: AllVariants::StructVar { flag: false, small: 8 },
        seq_unbounded: UnboundedSeq(&[10, 20]),
    };

    // -0.0 and +0.0 canonicalize identically
    let fp_pos0 = fingerprints::canonical_fp(&make(0.0));
    let fp_neg0 = fingerprints::canonical_fp(&make(-0.0));
    assert_eq!(fp_pos0, fp_neg0);
    assert_ne!(fp_pos0, fingerprints::canonical_fp(&BytesWrap(b"\x01\x02\x04")));

    // Map key-order independence via xor & sum
    let m1 = KVSlice(&[("alpha", 1), ("beta", 2), ("gamma", 3)]);
    let m2 = KVSlice(&[("gamma", 3), ("alpha", 1), ("beta", 2)]);
    assert_eq!(fingerprints::canonical_fp(&m1), fingerprints::canonical_fp(&m2));

    let mut m3_base = KVSlice(&[("alpha", 1), ("beta", 99), ("gamma", 3)]);
    assert_ne!(fingerprints::canonical_fp(&m1), fingerprints::canonical_fp(&m3_base));
    m3_base = KVSlice(&[("alpha", 1), ("beta", 2)]);
    assert_ne!(fingerprints::canonical_fp(&m1), fingerprints::canonical_fp(&m3_base));
}

#[test]
fn service_domains_store_hash_json_all_variants_and_corrupt_load() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    let mut snap = WorldStateSnapshot::default();
    // Populate JSON Null, Bool, Number, String, Array, Object in session_registry so hash_json_value covers all variants
    snap.cluster_and_runtime.session_registry.insert(
        "mix".into(),
        serde_json::json!({
            "null_v": null,
            "bool_v": true,
            "num_v": 42,
            "str_v": "hi",
            "arr_v": [1, false, null, "x", {"inner": 9}],
        }),
    );

    let mut fp = TableFingerprints::default();
    {
        let tx = conn.transaction().unwrap();
        assert!(service_domains_store::sync_service_domains(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    assert_ne!(fp.service_domains.cluster_and_runtime, 0);

    // Rollback restores mutated cluster_and_runtime
    let expected = snap.cluster_and_runtime.session_registry.clone();
    snap.cluster_and_runtime.session_registry.clear();
    service_domains_store::rollback_service_domains(&conn, &mut snap, &fp);
    assert_eq!(snap.cluster_and_runtime.session_registry, expected);

    // Corrupt rows in service_domain_state are gracefully skipped by load_service_domains
    conn.execute(
        "UPDATE service_domain_state SET payload_json = '{corrupt' WHERE domain_key = 'cluster_and_runtime'",
        [],
    )
    .unwrap();
    let mut loaded = WorldStateSnapshot::default();
    service_domains_store::load_service_domains(&conn, &mut loaded);
    assert!(loaded.cluster_and_runtime.session_registry.is_empty());

    // Dropping service_domain_state fails sync cleanly
    conn.execute("DROP TABLE service_domain_state", []).unwrap();
    let mut empty_fp = TableFingerprints::default();
    let tx = conn.transaction().unwrap();
    assert!(service_domains_store::sync_service_domains(&tx, &snap, &mut empty_fp).is_err());
}
