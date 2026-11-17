//! Mutation-killing unit tests for `sqlite_store` edge cases across `checked_json`,
//! `industry_market_pve_store`, `corp_crimewatch_store`, `ledger_store`,
//! `notes_lp_sequence_store`, `load`, and `sync`.

#![forbid(unsafe_code)]

use super::evedb_sqlite_cov_tests::build_populated_snapshot;
use super::evedb_sqlite_load_errors_and_schema_tests::open_migrated_conn;
use crate::sqlite_store::checked_json::canonical_fp;
use crate::sqlite_store::load::load_snapshot_from_sqlite;
use crate::sqlite_store::sync::{replace_sqlite_from_snapshot, sync_snapshot_diff_for_test};
use crate::types::{
    AccountRecord, AsteroidRecord, CharacterRecord, ContractRecord, CorporationRecord,
    InventoryItemRecord, MailRecord, MarketOrderRecord, MiningLedgerRecord, SkillQueueEntry,
    SystemDynamicState, UpwellStructureRecord, WalletJournalEntry,
};
use crate::IskCents;

#[test]
fn evedb_kills_all_remaining_sqlite_store_mutants() {
    let mut base = build_populated_snapshot();
    let cid = 90_000_001i64;
    base.licensed_skins.insert(90_000_002, vec![1]);

    // 1. Kill None vs Some(1) (`unwrap_or(-1) -> unwrap_or(1)`) in canonical hashes
    let ch = base.characters[&cid].clone();
    assert_ne!(
        canonical_fp(&CharacterRecord { alliance_id: None, ..ch.clone() }),
        canonical_fp(&CharacterRecord { alliance_id: Some(1), ..ch.clone() }),
    );
    assert_ne!(
        canonical_fp(&CharacterRecord { war_faction_id: None, ..ch.clone() }),
        canonical_fp(&CharacterRecord { war_faction_id: Some(1), ..ch.clone() }),
    );
    assert_ne!(
        canonical_fp(&CharacterRecord { station_id: None, ..ch.clone() }),
        canonical_fp(&CharacterRecord { station_id: Some(1), ..ch.clone() }),
    );
    assert_ne!(
        canonical_fp(&CharacterRecord { structure_id: None, ..ch.clone() }),
        canonical_fp(&CharacterRecord { structure_id: Some(1), ..ch.clone() }),
    );
    assert_ne!(
        canonical_fp(&CharacterRecord { fleet_id: None, ..ch.clone() }),
        canonical_fp(&CharacterRecord { fleet_id: Some(1), ..ch.clone() }),
    );
    let mut ch2 = ch.clone();
    ch2.loyalty_points.insert(1_000_125, 100);
    ch2.loyalty_points.insert(1_000_170, 200);
    assert_ne!(canonical_fp(&ch2), canonical_fp(&ch));
    let mut ch3 = ch;
    ch3.standings.insert(1_000_125, 0.5);
    ch3.standings.insert(1_000_170, 2.5);
    assert_ne!(canonical_fp(&ch3), canonical_fp(&base.characters[&cid]));

    let mut item = InventoryItemRecord {
        item_id: 901,
        type_id: 587,
        owner_id: cid,
        location_id: 60_003_760,
        quantity: 1,
        singleton: true,
        blueprint_runs_remaining: None,
        ..Default::default()
    };
    let item_none_fp = canonical_fp(&item);
    item.blueprint_runs_remaining = Some(1);
    assert_ne!(canonical_fp(&item), item_none_fp);
    item.dynamic_attributes.insert(10, 1.5);
    item.dynamic_attributes.insert(20, 2.5);
    let item_dyn_fp = canonical_fp(&item);
    item.dynamic_attributes.insert(20, 3.5);
    assert_ne!(canonical_fp(&item), item_dyn_fp);

    let c_base = ContractRecord {
        contract_id: 1,
        issuer_id: cid,
        acceptor_id: None,
        current_bidder_id: None,
        ..Default::default()
    };
    assert_ne!(
        canonical_fp(&ContractRecord { acceptor_id: Some(1), ..c_base.clone() }),
        canonical_fp(&c_base)
    );
    assert_ne!(
        canonical_fp(&ContractRecord { current_bidder_id: Some(1), ..c_base.clone() }),
        canonical_fp(&c_base)
    );

    let s_base = SystemDynamicState {
        solar_system_id: 30_000_142,
        ess_linked_character_id: None,
        sov_alliance_id: None,
        ..Default::default()
    };
    assert_ne!(
        canonical_fp(&SystemDynamicState { ess_linked_character_id: Some(1), ..s_base.clone() }),
        canonical_fp(&s_base)
    );
    assert_ne!(
        canonical_fp(&SystemDynamicState { sov_alliance_id: Some(1), ..s_base.clone() }),
        canonical_fp(&s_base)
    );

    // 2. Kill high-bit mutants in MarketOrderRecord and 2-entry CowMap canonical hashes
    let mo_base = MarketOrderRecord {
        order_id: -1,
        owner_id: -1,
        station_id: -1,
        region_id: u32::MAX,
        type_id: u32::MAX,
        is_bid: true,
        price: IskCents(-1),
        vol_entered: u32::MAX as i32,
        vol_remaining: u32::MAX as i32,
        min_volume: 1,
        is_npc: true,
        escrow: IskCents(-1),
    };
    let mo_fp = canonical_fp(&mo_base);
    assert_ne!(canonical_fp(&MarketOrderRecord { order_id: 1, ..mo_base.clone() }), mo_fp);
    assert_ne!(canonical_fp(&MarketOrderRecord { owner_id: 1, ..mo_base.clone() }), mo_fp);
    assert_ne!(canonical_fp(&MarketOrderRecord { station_id: 1, ..mo_base.clone() }), mo_fp);
    assert_ne!(canonical_fp(&MarketOrderRecord { region_id: 1, ..mo_base.clone() }), mo_fp);
    assert_ne!(canonical_fp(&MarketOrderRecord { price: IskCents(1), ..mo_base.clone() }), mo_fp);
    assert_ne!(canonical_fp(&MarketOrderRecord { vol_entered: 1, ..mo_base.clone() }), mo_fp);
    assert_ne!(canonical_fp(&MarketOrderRecord { escrow: IskCents(1), ..mo_base.clone() }), mo_fp);

    let mut orders = crate::CowMap::new();
    orders.insert(1, mo_base.clone());
    orders.insert(2, MarketOrderRecord { order_id: 2, price: IskCents(500), ..mo_base });
    let d1 = canonical_fp(&orders);
    orders.get_mut(&2).unwrap().price = IskCents(600);
    assert_ne!(canonical_fp(&orders), d1);

    // 3. Kill 2-entry XOR mutants in ChangeSet::diff domain_rows and cold_domains
    let mut base2 = base.clone();
    base2.licensed_skins.insert(90_000_002, vec![2, 3]);
    base2.paused_skill_queues.insert(90_000_001);
    base2.paused_skill_queues.insert(90_000_002);
    base2.weapon_banks.entry(100).or_default().insert(1, vec![10]);
    base2.weapon_banks.entry(200).or_default().insert(2, vec![20]);
    base2.mining_ledger.push(MiningLedgerRecord {
        character_id: 90_000_001,
        solar_system_id: 30_000_142,
        type_id: 1230,
        quantity: 100,
        date: "2026-01-01".into(),
        timestamp_unix: 1,
    });
    let mut base_acl_two = base2.clone();
    base2.structure_docking_acl_denied.entry(10).or_default().extend([1, 2]);
    base2.structure_docking_acl_denied.entry(20).or_default().extend([3, 4]);
    let diff_init = crate::tx_engine::ChangeSet::diff(1, &base_acl_two, &base2);
    base_acl_two = base2.clone();
    base2.paused_skill_queues.remove(&90_000_002);
    base2.paused_skill_queues.insert(90_000_003);
    base2.weapon_banks.get_mut(&200).unwrap().insert(2, vec![21]);
    base2.mining_ledger[0].quantity = 101;
    base2.structure_docking_acl_denied.get_mut(&20).unwrap().insert(99);
    let diff_acl_update = crate::tx_engine::ChangeSet::diff(2, &base_acl_two, &base2);
    assert!(diff_acl_update.cold_domains.d6_batch4);
    assert!(!diff_init.domain_rows.is_empty());
    assert_eq!(diff_acl_update.domain_rows.len(), 4);
    assert!(diff_acl_update.domain_rows.iter().any(|op| matches!(
        op,
        crate::sqlite_store::domain_rows::DomainRowChange::Upsert { domain, key, .. }
            if *domain == crate::sqlite_store::domain_rows::DOMAIN_CRIMEWATCH
                && key == "structure_docking_acl_denied/20"
    )));
    assert!(diff_acl_update.domain_rows.iter().any(|op| matches!(
        op,
        crate::sqlite_store::domain_rows::DomainRowChange::Delete { domain, key }
            if *domain == crate::sqlite_store::domain_rows::DOMAIN_SHIP_FITTING_AND_SKILLS
                && key == "paused_skill_queues/90000002"
    )));
    assert!(diff_acl_update.domain_rows.iter().any(|op| matches!(
        op,
        crate::sqlite_store::domain_rows::DomainRowChange::Upsert { domain, key, .. }
            if *domain == crate::sqlite_store::domain_rows::DOMAIN_SHIP_FITTING_AND_SKILLS
                && key == "paused_skill_queues/90000003"
    )));
    assert!(diff_acl_update.domain_rows.iter().any(|op| matches!(
        op,
        crate::sqlite_store::domain_rows::DomainRowChange::Upsert { domain, key, .. }
            if *domain == crate::sqlite_store::domain_rows::DOMAIN_SHIP_FITTING_AND_SKILLS
                && key == "weapon_banks/200"
    )));

    // 4. Kill world_meta single-field updates, next_entity_id load, same-length market_orders, append vs rewrite, and isolated cold_changed flags
    let mut conn = open_migrated_conn();
    let mut sync_snap = base.clone();
    sync_snap.wallet_journal.push(WalletJournalEntry {
        entry_id: 800_201,
        character_id: cid,
        entry_type: "Second".into(),
        amount: IskCents(50),
        balance_after: IskCents(150),
        reason: "r2".into(),
    });
    sync_snap.mails.push(MailRecord {
        message_id: 630_201,
        sender_id: cid,
        recipient_id: cid,
        subject: "S2".into(),
        body: "B2".into(),
    });
    replace_sqlite_from_snapshot(&mut conn, &sync_snap).expect("replace");
    let mut prev = sync_snap.clone();
    {
        let tx = conn.transaction().expect("warm tx");
        sync_snapshot_diff_for_test(&tx, &mut prev, &sync_snap, false).expect("warm sync");
        tx.commit().expect("warm commit");
    }

    for step in 0..3 {
        match step {
            0 => sync_snap.next_entity_id = 999_888_777,
            1 => sync_snap.wal_seq += 1,
            _ => sync_snap.markets_seeded = false,
        }
        let tx = conn.transaction().expect("tx");
        let delta = sync_snapshot_diff_for_test(&tx, &mut prev, &sync_snap, true).expect("sync");
        tx.commit().expect("commit");
        assert_eq!(delta.full_snapshot.is_some(), step == 2);
    }
    sync_snap.market_orders.get_mut(&700_100).unwrap().price = IskCents(777);
    {
        let tx = conn.transaction().expect("tx");
        sync_snapshot_diff_for_test(&tx, &mut prev, &sync_snap, false).expect("same-len order");
        tx.commit().expect("commit");
    }
    let loaded = load_snapshot_from_sqlite(&conn).expect("load");
    assert_eq!(loaded.next_entity_id, 999_888_777);
    assert_eq!(loaded.wal_seq, sync_snap.wal_seq);
    assert!(!loaded.markets_seeded);
    assert_eq!(loaded.market_orders[&700_100].price, IskCents(777));

    // Append 2 entries on top of 2 existing entries (`len=2 + idx=1 -> seq_idx=3`)
    sync_snap.wallet_journal.push(WalletJournalEntry {
        entry_id: 800_202,
        character_id: cid,
        entry_type: "Third".into(),
        amount: IskCents(10),
        balance_after: IskCents(160),
        reason: "r3".into(),
    });
    sync_snap.wallet_journal.push(WalletJournalEntry {
        entry_id: 800_203,
        character_id: cid,
        entry_type: "Fourth".into(),
        amount: IskCents(20),
        balance_after: IskCents(180),
        reason: "r4".into(),
    });
    {
        let tx = conn.transaction().expect("tx");
        let d = sync_snapshot_diff_for_test(&tx, &mut prev, &sync_snap, true).expect("append 2 journal");
        tx.commit().expect("commit");
        assert_eq!(d.appended_wallet_journal.len(), 2);
        assert!(d.full_snapshot.is_none());
    }
    sync_snap.mails.push(MailRecord {
        message_id: 630_202,
        sender_id: cid,
        recipient_id: cid,
        subject: "S3".into(),
        body: "B3".into(),
    });
    sync_snap.mails.push(MailRecord {
        message_id: 630_203,
        sender_id: cid,
        recipient_id: cid,
        subject: "S4".into(),
        body: "B4".into(),
    });
    {
        let tx = conn.transaction().expect("tx");
        let d = sync_snapshot_diff_for_test(&tx, &mut prev, &sync_snap, true).expect("append 2 mails");
        tx.commit().expect("commit");
        assert!(d.full_snapshot.is_some());
    }
    let loaded4 = load_snapshot_from_sqlite(&conn).expect("load 4");
    assert_eq!(loaded4.wallet_journal.len(), 4);
    assert_eq!(loaded4.wallet_journal[3].entry_id, 800_203);
    assert_eq!(loaded4.mails.len(), 4);
    assert_eq!(loaded4.mails[3].message_id, 630_203);

    // Clear wallet_journal & mails to empty (`len=0 < prev len=4`)
    sync_snap.wallet_journal.clear();
    sync_snap.mails.clear();
    {
        let tx = conn.transaction().expect("tx");
        let d = sync_snapshot_diff_for_test(&tx, &mut prev, &sync_snap, true).expect("clear");
        tx.commit().expect("commit");
        assert!(d.full_snapshot.is_some());
    }

    // Isolated cold_changed |= checks for extension, ledger, d6, and social sub-stores
    for sub_idx in 0..4 {
        match sub_idx {
            0 => {
                sync_snap.character_last_jump_unix.insert(cid, 9_999_999);
            }
            1 => {
                sync_snap.account_plex_vault.insert(1, 9_999);
            }
            2 => sync_snap.next_report_id += 1,
            _ => {
                sync_snap.read_mails.insert((cid, 999_001));
            }
        }
        let tx = conn.transaction().expect("tx");
        let d = sync_snapshot_diff_for_test(&tx, &mut prev, &sync_snap, true).expect("cold delta");
        tx.commit().expect("commit");
        assert!(d.full_snapshot.is_some(), "sub_idx {sub_idx} must set cold_changed");
    }
}

#[test]
fn fp_asteroid_sensitive_to_all_ten_fields() {
    let base = AsteroidRecord {
        asteroid_id: 70_000_001,
        solar_system_id: 30_000_142,
        belt_id: 40_009_082,
        type_id: 1230,
        quantity: 10_000,
        initial_quantity: 15_000,
        position: (1.5, -2.5, 3.5),
        radius: 125.0,
    };
    let h0 = canonical_fp(&base);
    assert_ne!(canonical_fp(&AsteroidRecord { asteroid_id: 70_000_002, ..base }), h0);
    assert_ne!(canonical_fp(&AsteroidRecord { solar_system_id: 30_000_143, ..base }), h0);
    assert_ne!(canonical_fp(&AsteroidRecord { belt_id: 40_009_083, ..base }), h0);
    assert_ne!(canonical_fp(&AsteroidRecord { type_id: 1228, ..base }), h0);
    assert_ne!(canonical_fp(&AsteroidRecord { quantity: 9_999, ..base }), h0);
    assert_ne!(canonical_fp(&AsteroidRecord { initial_quantity: 15_001, ..base }), h0);
    assert_ne!(canonical_fp(&AsteroidRecord { position: (1.6, -2.5, 3.5), ..base }), h0);
    assert_ne!(canonical_fp(&AsteroidRecord { position: (1.5, -2.6, 3.5), ..base }), h0);
    assert_ne!(canonical_fp(&AsteroidRecord { position: (1.5, -2.5, 3.6), ..base }), h0);
    assert_ne!(canonical_fp(&AsteroidRecord { radius: 125.5, ..base }), h0);
}

#[test]
fn canonical_fingerprints_distinguish_none_from_zero_sentinel_and_extra_fields() {
    let acc_none = AccountRecord {
        user_id: 1,
        username: "u".into(),
        password_token: "p".into(),
        role: 0,
        banned_until_unix: None,
        ban_reason: None,
    };
    let acc_zero = AccountRecord {
        banned_until_unix: Some(0),
        ..acc_none.clone()
    };
    assert_ne!(canonical_fp(&acc_none), canonical_fp(&acc_zero));

    let snap = build_populated_snapshot();
    let item_base = snap.items.values().next().unwrap().clone();
    let item_muta0 = InventoryItemRecord {
        source_mutaplasmid_id: Some(0),
        ..item_base.clone()
    };
    assert_ne!(
        canonical_fp(&InventoryItemRecord {
            source_mutaplasmid_id: None,
            ..item_base.clone()
        }),
        canonical_fp(&item_muta0)
    );

    let item_bpc_none = InventoryItemRecord {
        blueprint_runs_remaining: None,
        ..item_base.clone()
    };
    let item_bpc_neg1 = InventoryItemRecord {
        blueprint_runs_remaining: Some(-1),
        ..item_base.clone()
    };
    assert_ne!(canonical_fp(&item_bpc_none), canonical_fp(&item_bpc_neg1));

    let c_base = snap.contracts.values().next().unwrap().clone();
    assert_ne!(
        canonical_fp(&ContractRecord { acceptor_id: None, ..c_base.clone() }),
        canonical_fp(&ContractRecord { acceptor_id: Some(-1), ..c_base.clone() })
    );
    assert_ne!(
        canonical_fp(&c_base),
        canonical_fp(&ContractRecord {
            for_corp: !c_base.for_corp,
            ..c_base
        })
    );

    let st_base = snap.structures.values().next().unwrap().clone();
    let st_zero = UpwellStructureRecord {
        self_destruct_at_unix: Some(0),
        ..st_base.clone()
    };
    assert_ne!(
        canonical_fp(&UpwellStructureRecord {
            self_destruct_at_unix: None,
            ..st_base
        }),
        canonical_fp(&st_zero)
    );

    let sq_none = vec![SkillQueueEntry {
        type_id: 3402,
        target_level: 1,
        start_sp: 0.0,
        end_sp: 250.0,
        duration_seconds: 60.0,
        started_at_unix: None,
    }];
    let sq_zero = vec![SkillQueueEntry {
        started_at_unix: Some(0),
        ..sq_none[0].clone()
    }];
    assert_ne!(canonical_fp(&sq_none), canonical_fp(&sq_zero));

    let mut c1 = CorporationRecord::new(98_001, "Alpha", "ALP", 90_001);
    let mut c2 = CorporationRecord::new(98_001, "Alpha", "ALP", 90_001);
    for id in [10, 20, 30, 40, 50] {
        c1.members.insert(id);
    }
    for id in [50, 30, 10, 40, 20] {
        c2.members.insert(id);
    }
    assert_eq!(canonical_fp(&c1), canonical_fp(&c2));
}
