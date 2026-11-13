//! Full SQLite relational + extension + WAL roundtrip, update, delete, and rollback tests.

use super::*;
use crate::clock::VirtualServerClock;
use crate::ledger::{JournalContext, JournalEntryInput, JournalMeta, MarketFill, ref_types};
use crate::snapshot_ext::{
    CalendarEventRecord, ChatChannelAclRecord, LpRewardLogEntry, MailingListRecord,
    SocialContactRecord,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn populate_all_domains(snap: &mut WorldStateSnapshot, cid: i64, alt_cid: i64, corp_id: i64, ally_id: i64) {
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;

    // Sequence counters & owner notes / LP / SP6 lifecycle
    snap.next_alliance_id = 99_000_111;
    snap.next_corporation_id = 98_100_222;
    snap.next_notification_id = 500_333;
    snap.next_report_id = 700_444;
    snap.owner_notes.insert(
        1,
        OwnerNoteRecord {
            note_id: 1,
            character_id: cid,
            item_id: 100_000_001,
            label: "Fit".into(),
            note: "MWD Scram Web".into(),
            timestamp_unix: 1_700_000_100,
        },
    );
    snap.corp_lp_balances.insert((corp_id, 1_000_035), 12_500);
    snap.char_lp_logs.insert(
        cid,
        vec![LpRewardLogEntry {
            log_id: 10,
            corp_id: 1_000_035,
            amount: 250,
            reason: "Mission".into(),
            timestamp_unix: 1_700_000_200,
        }],
    );
    snap.sp6_lifecycle = Sp6PersistedState {
        safe_logoffs: HashMap::from([(
            cid,
            SafeLogoffEntry {
                char_id: cid,
                ship_id: 100_000_001,
                solar_system_id: sys_id,
                started_unix: 1_700_000_900,
                complete_at_unix: 1_700_000_999,
                completed: false,
            },
        )]),
        disconnected_ships: HashMap::from([(
            cid,
            DisconnectedShipEntry {
                char_id: cid,
                ship_id: 100_000_001,
                ship_type_id: 587,
                solar_system_id: sys_id,
                disconnected_at_unix: 1_700_000_800,
                remove_at_unix: 1_700_000_888,
                emergency_warp_dest: Some([100.0, 200.0, 300.0]),
                last_position: [1.0, 2.0, 3.0],
                removed_from_space: false,
                pre_warp_position: Some([0.0, 0.0, 0.0]),
            },
        )]),
        self_destructs: HashMap::from([(
            100_000_001,
            SelfDestructPersistedEntry {
                ship_id: 100_000_001,
                char_id: cid,
                solar_system_id: sys_id,
                started_unix: 1_700_000_650,
                detonate_at_unix: 1_700_000_777,
            },
        )]),
        abyssal_sessions: HashMap::from([(
            cid,
            AbyssalPersistedSession {
                char_id: cid,
                ship_id: 100_000_001,
                pocket_system_id: 32_000_001,
                origin_system_id: sys_id,
                entered_unix: 1_700_000_000,
                collapse_at_unix: 1_700_001_200,
            },
        )]),
        pvp_timers_until: HashMap::from([(cid, 1_700_000_900)]),
        npc_timers_until: HashMap::from([(cid, 1_700_000_300)]),
        weapon_timers_until: HashMap::from([(cid, 1_700_000_060)]),
        criminal_timers_until: HashMap::from([(cid, 1_700_000_900)]),
        last_downtime_day_idx: Some(19_723),
        ship_combat_hp: HashMap::from([(
            100_000_001,
            PersistedCombatHp {
                shield_hp: 400.0,
                armor_hp: 300.0,
                hull_hp: 250.0,
            },
        )]),
        space_object_meta: HashMap::from([(
            200_050_001,
            PersistedSpaceObjectMeta {
                created_unix: Some(1_700_000_050),
                is_npc_wreck: false,
                anchor_completed: true,
                ..PersistedSpaceObjectMeta::default()
            },
        )]),
        missions_awaiting_downtime_respawn: vec![(cid, 101)],
        ..Sp6PersistedState::default()
    };

    // Ledger & market fill history
    let tx_id = snap.journal_market_fill(MarketFill {
        buyer_id: cid,
        seller_id: alt_cid,
        type_id: 34,
        quantity: 50,
        price: IskCents::from_isk(10),
        location_id: station_id,
        date_unix: 1_704_067_200,
        gross: IskCents::from_isk(500),
        buyer_balance_after: Some(IskCents::from_isk(4_999_500)),
        seller_net: IskCents::from_isk(470),
        seller_balance_after: Some(IskCents::from_isk(2_000_470)),
        sales_tax: IskCents::from_isk(25),
        tax_collector_id: 1_000_035,
        region_id: Some(10_000_002),
    });
    let _ = snap.append_journal_entry(JournalEntryInput {
        owner_id: corp_id,
        entry_type: "CorpTax",
        amount: IskCents::from_isk(1_000),
        balance_after: IskCents::from_isk(2_000_000),
        reason: "Tax".into(),
        meta: JournalMeta::new(ref_types::CORPORATION_ACCOUNT_WITHDRAWAL, cid, corp_id, 1_704_067_210)
            .with_context(JournalContext::MarketTransaction, tx_id),
    });

    // Extension maps (9 maps)
    snap.character_last_jump_unix.insert(cid, 1_700_000_111);
    snap.character_cspa_charges.insert(cid, IskCents::from_isk(2_950));
    snap.mail_timestamps.insert(930_001, 1_700_000_222);
    snap.character_login_stats.insert(
        cid,
        CharacterLoginStats {
            last_login_unix: 1_700_000_333,
            last_logout_unix: 1_700_000_444,
            logins_count: 7,
        },
    );
    snap.item_custom_names.insert(100_000_001, "My Ship".into());
    snap.item_positions.insert(100_000_001, [10.0, 20.0, 30.0]);
    snap.structure_vulnerability_windows.insert(970_001, (100, 200));
    snap.skill_history.insert(
        cid,
        vec![SkillHistoryEntry {
            log_date_ft: 133_500_000_000_000_000,
            event_type_id: 34,
            skill_type_id: 3300,
            skill_level: 5,
            absolute_points: 256_000.0,
        }],
    );
    snap.character_doll_info.insert(cid, ("dna_a".into(), "meta_a".into()));

    // Social store tables
    snap.read_mails.insert((cid, 930_001));
    snap.deleted_mails.insert((alt_cid, 930_001));
    snap.trashed_mails.insert((alt_cid, 930_002));
    snap.mail_labels.insert(
        cid,
        vec![MailLabelRecord {
            label_id: 1,
            name: "Important".into(),
            color: "#00ff00".into(),
        }],
    );
    snap.mail_assigned_labels.insert((cid, 930_001), vec![1]);
    snap.mailing_lists.insert(
        801,
        MailingListRecord {
            list_id: 801,
            name: "FleetComms".into(),
            owner_id: cid,
            default_access: 1,
            default_role: 1,
            cost: 0,
            members: HashMap::from([(cid, 3)]),
            entity_access: HashMap::from([(corp_id, 1)]),
            welcome_mail: Some(("Welcome".into(), "Fly safe".into())),
            mail_ids: HashSet::from([930_001]),
        },
    );
    snap.calendar_events.insert(
        851,
        CalendarEventRecord {
            event_id: 851,
            owner_id: cid,
            creator_id: cid,
            event_date_time: 1_704_100_000,
            duration: 60,
            importance: 1,
            title: "Roam".into(),
            description: "Formup Jita".into(),
            scope: "character".into(),
            responses: HashMap::from([(cid, 1)]),
            is_deleted: false,
        },
    );
    snap.bookmark_folders.insert(
        601,
        BookmarkFolderRecord {
            folder_id: 601,
            owner_id: cid,
            folder_name: "Safe Spots".into(),
            ..BookmarkFolderRecord::default()
        },
    );
    snap.bookmark_subfolders.insert(960_001, 601);
    snap.bookmark_subfolder_parents.insert(601, 600);
    snap.bookmark_expiries.insert(960_001, 1_705_000_000);
    snap.bookmark_folder_acls.insert(601, vec![10]);
    snap.bookmark_folder_known_by.insert((601, alt_cid), true);
    snap.access_groups.insert(10, vec![cid, alt_cid]);
    snap.chat_channels.insert(
        "player_chan_1".into(),
        ChatChannelRecord {
            channel_id: "player_chan_1".into(),
            name: "Custom Lounge".into(),
            motd: "Welcome".into(),
            owner_id: Some(cid),
            members: HashSet::from([cid]),
            is_system_local: false,
            solar_system_id: None,
            is_private: true,
            operators: HashSet::from([cid]),
            password: Some("pw".into()),
            is_delayed_local: false,
        },
    );
    snap.chat_channel_acl.insert(
        "player_chan_1".into(),
        ChatChannelAclRecord {
            allow_list: HashSet::from([cid, alt_cid]),
            deny_list: HashSet::new(),
            muted_until: HashMap::new(),
        },
    );
    snap.char_contacts.insert(
        cid,
        vec![SocialContactRecord {
            contact_id: alt_cid,
            contact_type: "character".into(),
            standing: 5.0,
            is_watched: true,
            is_blocked: false,
            label_ids: vec![1],
        }],
    );
    snap.char_contact_labels.insert(cid, vec![(1, "Friends".into())]);
    snap.corp_contacts.insert(
        corp_id,
        vec![SocialContactRecord {
            contact_id: alt_cid,
            contact_type: "character".into(),
            standing: 10.0,
            is_watched: false,
            is_blocked: false,
            label_ids: vec![],
        }],
    );
    snap.corp_contact_labels.insert(corp_id, vec![(1, "Allies".into(), 255)]);
    snap.alliance_contacts.insert(
        ally_id,
        vec![SocialContactRecord {
            contact_id: corp_id,
            contact_type: "corporation".into(),
            standing: 10.0,
            is_watched: false,
            is_blocked: false,
            label_ids: vec![],
        }],
    );
    snap.alliance_contact_labels.insert(ally_id, vec![(1, "Blues".into())]);
    snap.char_notes.insert((cid, 100_000_001), "Main frigate".into());

    // Corp Governance, Crimewatch, Warfare
    snap.corp_member_limits.insert(corp_id, 250);
    snap.corp_hangar_divisions.insert(corp_id, HashMap::from([(1, "Main Hangar".into())]));
    snap.zarzakh_gate_locks.insert(cid, (30_000_142, 1_700_005_000));
    snap.structure_docking_acl_denied.insert(970_001, HashSet::from([alt_cid]));
    snap.sov_prospecting_levels.insert(sys_id, 4);
    snap.fw_corp_enlisted_on.insert(corp_id, 1_700_000_000);
    snap.fw_wars.push((500_001, 500_004));

    // Industry, Market, PvE store
    snap.module_online_states.insert(100_000_001, true);
    snap.module_active_states.insert(100_000_001);
    snap.module_active_charges.insert(100_000_001, 185);
    snap.weapon_banks.insert(100_000_001, HashMap::from([(1, vec![100_000_001])]));
    snap.paused_skill_queues.insert(alt_cid);
    snap.wh_polarizations.insert((cid, 50_000_777), 1_700_001_000);
    snap.last_mission_decline_times.insert(cid, 1_700_000_050);
    snap.orbital_provision_types.insert(70_000_001, 2233);
    snap.quasar_state_blobs.insert("entitlements".into(), "{}".into());
    snap.service_state_blobs.insert("extra".into(), "1".into());
}

#[test]
fn sqlite_store_insert_update_rollback_delete_and_wal_recovery() {
    let pid = std::process::id();
    let db_path = std::env::temp_dir().join(format!("evomu_cov_roundtrip_{pid}.sqlite"));
    let _ = std::fs::remove_file(&db_path);

    let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");
    let demo_chars = WorldRepository::create_demo_snapshot().characters;
    let cid = 90_000_001_i64;
    let alt_cid = 90_000_002_i64;
    let corp_id = 98_000_099_i64;
    let ally_id = 99_000_099_i64;

    // 1. Insert populated domain state & commit to SQLite
    repo.with_mut_snapshot(|snap| {
        let mut c1 = demo_chars[&cid].clone();
        c1.corporation_id = corp_id;
        c1.balance = IskCents::from_isk(5_000_000);
        snap.characters.insert(cid, c1);
        snap.characters.insert(alt_cid, demo_chars[&alt_cid].clone());
        snap.items.insert(
            100_000_001,
            InventoryItemRecord {
                item_id: 100_000_001,
                type_id: 587,
                owner_id: cid,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Cov Corp".into(),
                ticker: "COVC".into(),
                ceo_id: cid,
                master_wallet: IskCents::from_isk(1_000_000),
                hq_station_id: Some(60_003_760),
                alliance_id: Some(ally_id),
                ..CorporationRecord::default()
            },
        );
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "Cov Alliance".into(),
                short_name: "COVA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );
        snap.fleets.insert(
            940_001,
            FleetRecord {
                fleet_id: 940_001,
                boss_id: cid,
                motd: "Welcome".into(),
                free_move: true,
                members: vec![cid],
                ..FleetRecord::default()
            },
        );
        populate_all_domains(snap, cid, alt_cid, corp_id, ally_id);
    });

    // 2. Update existing rows in-place (exercises UPDATE paths across sync modules)
    repo.with_mut_snapshot(|snap| {
        snap.owner_notes.get_mut(&1).unwrap().note = "Updated note".into();
        snap.corp_lp_balances.insert((corp_id, 1_000_035), 25_000);
        snap.char_lp_logs.get_mut(&cid).unwrap()[0].amount = 500;
        snap.sp6_lifecycle.ship_combat_hp.get_mut(&100_000_001).unwrap().shield_hp = 123.0;
        snap.character_cspa_charges.insert(cid, IskCents::from_isk(1_000));
        snap.character_doll_info.insert(cid, ("dna_b".into(), "meta_b".into()));
        snap.corp_member_limits.insert(corp_id, 500);
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: cid,
            solar_system_id: 30_000_142,
            type_id: 1230,
            quantity: 777,
            date: "2024-01-01".into(),
            timestamp_unix: 1_704_067_200,
        });
    });

    // 3. Corrupt every domain inside a failing transaction and verify complete SQLite rollback!
    let rb: Result<(), DbError> = repo.try_with_mut_snapshot(|snap| {
        snap.next_alliance_id = 1;
        snap.owner_notes.clear();
        snap.corp_lp_balances.clear();
        snap.char_lp_logs.clear();
        snap.sp6_lifecycle = Sp6PersistedState::default();
        snap.account_plex_vault.clear();
        snap.wallet_journal_meta.clear();
        snap.market_transactions.clear();
        snap.char_contacts.clear();
        snap.mailing_lists.clear();
        snap.calendar_events.clear();
        snap.chat_channels.clear();
        snap.corp_member_limits.clear();
        snap.zarzakh_gate_locks.clear();
        snap.fw_wars.clear();
        snap.mining_ledger.clear();
        snap.module_online_states.clear();
        snap.quasar_state_blobs.clear();
        snap.service_state_blobs.clear();
        Err(DbError::ValidationError("rollback test".into()))
    });
    assert!(rb.is_err());

    repo.with_snapshot(|snap| {
        assert_eq!(snap.next_alliance_id, 99_000_111);
        assert_eq!(snap.owner_notes[&1].note, "Updated note");
        assert_eq!(snap.corp_lp_balances[&(corp_id, 1_000_035)], 25_000);
        assert_eq!(snap.sp6_lifecycle.ship_combat_hp[&100_000_001].shield_hp, 123.0);
        assert_eq!(snap.character_doll_info[&cid].0, "dna_b");
        assert_eq!(snap.corp_member_limits[&corp_id], 500);
        assert_eq!(snap.mining_ledger.len(), 1);
        assert_eq!(snap.fw_wars, vec![(500_001, 500_004)]);
    });

    // 4. Delete rows from all maps & tables and commit (exercises DELETE paths across sync modules)
    repo.with_mut_snapshot(|snap| {
        snap.owner_notes.clear();
        snap.corp_lp_balances.clear();
        snap.char_lp_logs.clear();
        snap.character_last_jump_unix.clear();
        snap.character_cspa_charges.clear();
        snap.mail_timestamps.clear();
        snap.character_login_stats.clear();
        snap.item_custom_names.clear();
        snap.item_positions.clear();
        snap.structure_vulnerability_windows.clear();
        snap.skill_history.clear();
        snap.character_doll_info.clear();
        snap.read_mails.clear();
        snap.deleted_mails.clear();
        snap.trashed_mails.clear();
        snap.mail_labels.clear();
        snap.mail_assigned_labels.clear();
        snap.mailing_lists.clear();
        snap.calendar_events.clear();
        snap.bookmark_folders.clear();
        snap.access_groups.clear();
        snap.chat_channels.clear();
        snap.char_contacts.clear();
        snap.corp_contacts.clear();
        snap.alliance_contacts.clear();
        snap.char_notes.clear();
        snap.fleets.clear();
        snap.alliances.clear();
        snap.items.clear();
    });

    // Reopen SQLite from disk to verify persisted state reload.
    let copy_sqlite = std::env::temp_dir().join(format!("evomu_cov_save_copy_{pid}.sqlite"));
    let _ = std::fs::remove_file(&copy_sqlite);
    repo.save_to_sqlite(&copy_sqlite).expect("save_to_sqlite");
    drop(repo);

    let reopened = WorldRepository::open_sqlite(&copy_sqlite).expect("reopen saved copy");
    reopened.with_snapshot(|snap| {
        assert_eq!(snap.next_alliance_id, 99_000_111);
        assert!(snap.owner_notes.is_empty());
        assert_eq!(snap.mining_ledger.len(), 1);
    });
    drop(reopened);

    // 5. JSON snapshot + WAL fault injection (modes 1 & 2) + graceful Drop compaction + crash torn-tail healing
    let wal_dir = std::env::temp_dir().join(format!("evomu_cov_wal_dir_{pid}"));
    let _ = std::fs::remove_dir_all(&wal_dir);
    std::fs::create_dir_all(&wal_dir).expect("create wal tempdir");
    let snap_path = wal_dir.join("evomu_cov_wal.json");
    let wal_path = WorldRepository::wal_path_for(&snap_path);

    let json_repo =
        WorldRepository::new_empty_with_snapshot_path(Some(snap_path.clone())).expect("init json repo");
    assert!(!json_repo.is_ephemeral_in_memory());
    let clk = Arc::new(VirtualServerClock::with_fixed_base(1_700_000_000));
    json_repo.set_clock(clk);
    assert_eq!(json_repo.clock().now_unix_secs(), 1_700_000_000);
    assert!(json_repo.try_with_snapshot(|s| s.wal_seq).is_some());

    // Normal WAL append + write lock depth check
    json_repo.with_mut_snapshot(|snap| {
        assert!(json_repo.is_write_locked_by_current_thread());
        snap.committed_idempotency_keys.insert((1, "k1".into()));
    });
    assert!(!json_repo.is_write_locked_by_current_thread());
    assert!(wal_path.exists());

    // TxFaultPlan FailNthStatement & FailAtCommit roll back cleanly
    json_repo.set_fault_plan_for_test(crate::TxFaultPlan::FailNthStatement(1));
    let err1: Result<(), DbError> = json_repo.try_with_mut_snapshot(|snap| {
        snap.committed_idempotency_keys.insert((1, "k_fail1".into()));
        Ok(())
    });
    assert!(err1.is_err());

    json_repo.set_fault_plan_for_test(crate::TxFaultPlan::FailAtCommit);
    let err2: Result<(), DbError> = json_repo.try_with_mut_snapshot(|snap| {
        snap.committed_idempotency_keys.insert((1, "k_fail2".into()));
        Ok(())
    });
    assert!(err2.is_err());
    // Graceful Drop compacts pending WAL records into snap_path.
    drop(json_repo);
    assert!(!wal_path.exists());

    // Reopen, write k2 into WAL, simulate crash via mem::forget, and append torn trailing WAL record.
    let crash_repo =
        WorldRepository::new_empty_with_snapshot_path(Some(snap_path.clone())).expect("reopen json repo");
    crash_repo.with_mut_snapshot(|snap| {
        snap.committed_idempotency_keys.insert((1, "k2".into()));
    });
    std::mem::forget(crash_repo);

    use std::io::Write as _;
    {
        let mut f = std::fs::OpenOptions::new().append(true).open(&wal_path).unwrap();
        f.write_all(b"{\"seq\":999,\"truncated_json").unwrap();
    }
    let healed_repo =
        WorldRepository::new_empty_with_snapshot_path(Some(snap_path.clone())).expect("heal torn tail");
    healed_repo.with_snapshot(|s| {
        assert!(s.committed_idempotency_keys.contains(&(1, "k1".into())));
        assert!(s.committed_idempotency_keys.contains(&(1, "k2".into())));
    });

    // Non-trailing corrupt line in WAL -> load_snapshot_from_file returns Err
    std::fs::write(&wal_path, b"corrupt_line\n{\"seq\":2}\n").unwrap();
    assert!(healed_repo.load_snapshot_from_file(&snap_path).is_err());

    let _ = std::fs::remove_dir_all(&wal_dir);

    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(db_path.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(db_path.with_extension("sqlite-shm"));
    let _ = std::fs::remove_file(&copy_sqlite);
    let _ = std::fs::remove_file(copy_sqlite.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(copy_sqlite.with_extension("sqlite-shm"));
    let _ = std::fs::remove_file(&snap_path);
    let _ = std::fs::remove_file(&wal_path);
}
