//! Mutation-killing unit tests for `sqlite_store` edge cases across `fingerprints`,
//! `industry_market_pve_store`, `corp_crimewatch_store`, `rollback`, `ledger_store`,
//! `notes_lp_sequence_store`, `load`, and `sync`.

#![forbid(unsafe_code)]

use super::evedb_sqlite_cov_tests::build_populated_snapshot;
use super::evedb_sqlite_load_errors_and_schema_tests::open_migrated_conn;
use crate::sqlite_store::corp_crimewatch_store::compute_d6_corp_crimewatch_fingerprints;
use crate::sqlite_store::fingerprints::{
    compute_fingerprints, fp_asteroid, fp_character, fp_contract, fp_item, fp_market_order,
    fp_market_orders_digest, fp_system_state,
};
use crate::sqlite_store::industry_market_pve_store::compute_d6_batch4_fingerprints;
use crate::sqlite_store::ledger_store::rollback_ledger;
use crate::sqlite_store::load::load_snapshot_from_sqlite;
use crate::sqlite_store::notes_lp_sequence_store::rollback_d6;
use crate::sqlite_store::rollback::rollback_dirty_from_sqlite;
use crate::sqlite_store::sync::{replace_sqlite_from_snapshot, sync_dirty_to_sqlite};
use crate::types::{
    AccountRecord, AsteroidRecord, CharacterRecord, ContractRecord, CorporationRecord,
    InventoryItemRecord, MailRecord, MarketOrderRecord, SkillQueueEntry, SystemDynamicState,
    UpwellStructureRecord, WalletJournalEntry,
};
use crate::IskCents;

#[test]
fn evedb_kills_all_remaining_sqlite_store_mutants() {
    let mut base = build_populated_snapshot();
    let cid = 90_000_001i64;
    base.licensed_skins.insert(90_000_002, vec![1]);

    // 1. Kill None vs Some(1) (`unwrap_or(-1) -> unwrap_or(1)`) in fingerprints
    let ch = base.characters[&cid].clone();
    assert_ne!(
        fp_character(&CharacterRecord { alliance_id: None, ..ch.clone() }),
        fp_character(&CharacterRecord { alliance_id: Some(1), ..ch.clone() }),
    );
    assert_ne!(
        fp_character(&CharacterRecord { war_faction_id: None, ..ch.clone() }),
        fp_character(&CharacterRecord { war_faction_id: Some(1), ..ch.clone() }),
    );
    assert_ne!(
        fp_character(&CharacterRecord { station_id: None, ..ch.clone() }),
        fp_character(&CharacterRecord { station_id: Some(1), ..ch.clone() }),
    );
    assert_ne!(
        fp_character(&CharacterRecord { structure_id: None, ..ch.clone() }),
        fp_character(&CharacterRecord { structure_id: Some(1), ..ch.clone() }),
    );
    assert_ne!(
        fp_character(&CharacterRecord { fleet_id: None, ..ch.clone() }),
        fp_character(&CharacterRecord { fleet_id: Some(1), ..ch.clone() }),
    );
    let mut ch2 = ch.clone();
    ch2.loyalty_points.insert(1_000_125, 100);
    ch2.loyalty_points.insert(1_000_170, 200);
    assert_ne!(fp_character(&ch2), fp_character(&ch));
    let mut ch3 = ch;
    ch3.standings.insert(1_000_125, 0.5);
    ch3.standings.insert(1_000_170, 2.5);
    assert_ne!(fp_character(&ch3), fp_character(&base.characters[&cid]));

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
    let item_none_fp = fp_item(&item);
    item.blueprint_runs_remaining = Some(1);
    assert_ne!(fp_item(&item), item_none_fp);
    item.dynamic_attributes.insert(10, 1.5);
    item.dynamic_attributes.insert(20, 2.5);
    let item_dyn_fp = fp_item(&item);
    item.dynamic_attributes.insert(20, 3.5);
    assert_ne!(fp_item(&item), item_dyn_fp);

    let c_base = ContractRecord {
        contract_id: 1,
        issuer_id: cid,
        acceptor_id: None,
        current_bidder_id: None,
        ..Default::default()
    };
    assert_ne!(
        fp_contract(&ContractRecord { acceptor_id: Some(1), ..c_base.clone() }),
        fp_contract(&c_base)
    );
    assert_ne!(
        fp_contract(&ContractRecord { current_bidder_id: Some(1), ..c_base.clone() }),
        fp_contract(&c_base)
    );

    let s_base = SystemDynamicState {
        solar_system_id: 30_000_142,
        ess_linked_character_id: None,
        sov_alliance_id: None,
        ..Default::default()
    };
    assert_ne!(
        fp_system_state(&SystemDynamicState { ess_linked_character_id: Some(1), ..s_base.clone() }),
        fp_system_state(&s_base)
    );
    assert_ne!(
        fp_system_state(&SystemDynamicState { sov_alliance_id: Some(1), ..s_base.clone() }),
        fp_system_state(&s_base)
    );

    // 2. Kill high-bit XOR/OR/AND mutants in fp_market_order and 2-entry fp_market_orders_digest
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
    let mo_fp = fp_market_order(&mo_base);
    assert_ne!(fp_market_order(&MarketOrderRecord { order_id: 1, ..mo_base.clone() }), mo_fp);
    assert_ne!(fp_market_order(&MarketOrderRecord { owner_id: 1, ..mo_base.clone() }), mo_fp);
    assert_ne!(fp_market_order(&MarketOrderRecord { station_id: 1, ..mo_base.clone() }), mo_fp);
    assert_ne!(fp_market_order(&MarketOrderRecord { region_id: 1, ..mo_base.clone() }), mo_fp);
    assert_ne!(fp_market_order(&MarketOrderRecord { price: IskCents(1), ..mo_base.clone() }), mo_fp);
    assert_ne!(fp_market_order(&MarketOrderRecord { vol_entered: 1, ..mo_base.clone() }), mo_fp);
    assert_ne!(fp_market_order(&MarketOrderRecord { escrow: IskCents(1), ..mo_base.clone() }), mo_fp);

    let mut orders = std::collections::HashMap::new();
    orders.insert(1, mo_base.clone());
    orders.insert(2, MarketOrderRecord { order_id: 2, price: IskCents(500), ..mo_base });
    let d1 = fp_market_orders_digest(&orders);
    orders.get_mut(&2).unwrap().price = IskCents(600);
    assert_ne!(fp_market_orders_digest(&orders), d1);

    // 3. Kill 2-entry XOR mutants in hash_map_xor, batch4, and corp_crimewatch
    let mut base2 = base.clone();
    base2.licensed_skins.insert(90_000_002, vec![2, 3]);
    assert_ne!(compute_fingerprints(&base2).licensed_skins, compute_fingerprints(&base).licensed_skins);

    base2.paused_skill_queues.insert(90_000_001);
    base2.paused_skill_queues.insert(90_000_002);
    base2.weapon_banks.entry(100).or_default().insert(1, vec![10]);
    base2.weapon_banks.entry(200).or_default().insert(2, vec![20]);
    base2.structure_docking_acl_denied.entry(10).or_default().extend([1, 2]);
    base2.structure_docking_acl_denied.entry(20).or_default().extend([3, 4]);
    let b4_a = compute_d6_batch4_fingerprints(&base2);
    let cw_a = compute_d6_corp_crimewatch_fingerprints(&base2);
    base2.paused_skill_queues.remove(&90_000_002);
    base2.paused_skill_queues.insert(90_000_003);
    base2.weapon_banks.get_mut(&200).unwrap().insert(2, vec![21]);
    base2.structure_docking_acl_denied.get_mut(&20).unwrap().insert(99);
    let b4_b = compute_d6_batch4_fingerprints(&base2);
    let cw_b = compute_d6_corp_crimewatch_fingerprints(&base2);
    assert_ne!(b4_a.blobs["paused_skill_queues"], b4_b.blobs["paused_skill_queues"]);
    assert_ne!(b4_a.blobs["weapon_banks"], b4_b.blobs["weapon_banks"]);
    assert_ne!(cw_a.crimewatch_fp, cw_b.crimewatch_fp);

    // 4. Kill clean no-op rollback guards across rollback_dirty_from_sqlite, rollback_d6, rollback_ledger
    let empty_conn = open_migrated_conn();
    let clean_fp = compute_fingerprints(&base);
    let mut clean_snap = base.clone();
    rollback_dirty_from_sqlite(&empty_conn, &mut clean_snap, &clean_fp).expect("clean rollback");
    rollback_d6(&empty_conn, &mut clean_snap, &clean_fp);
    rollback_ledger(&empty_conn, &mut clean_snap, &clean_fp);
    assert_eq!(clean_snap.accounts.len(), base.accounts.len());
    assert_eq!(clean_snap.characters.len(), base.characters.len());
    assert_eq!(clean_snap.skills.len(), base.skills.len());
    assert_eq!(clean_snap.skill_queues.len(), base.skill_queues.len());
    assert_eq!(clean_snap.items.len(), base.items.len());
    assert_eq!(clean_snap.market_orders.len(), base.market_orders.len());
    assert_eq!(clean_snap.wallet_journal.len(), base.wallet_journal.len());
    assert_eq!(clean_snap.mails.len(), base.mails.len());
    assert_eq!(clean_snap.structures.len(), base.structures.len());
    assert_eq!(clean_snap.owner_notes.len(), base.owner_notes.len());
    assert_eq!(clean_snap.corp_lp_balances.len(), base.corp_lp_balances.len());
    assert_eq!(clean_snap.account_plex_vault.len(), base.account_plex_vault.len());

    // 5. Kill world_meta single-field updates, next_entity_id load, same-length market_orders, append vs rewrite, and isolated cold_changed flags
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
    let mut fp = replace_sqlite_from_snapshot(&mut conn, &sync_snap).expect("replace");
    {
        let tx = conn.transaction().expect("warm tx");
        sync_dirty_to_sqlite(&tx, &sync_snap, &mut fp, false).expect("warm sync");
        tx.commit().expect("warm commit");
    }

    for step in 0..3 {
        match step {
            0 => sync_snap.next_entity_id = 999_888_777,
            1 => sync_snap.wal_seq += 1,
            _ => sync_snap.markets_seeded = false,
        }
        let tx = conn.transaction().expect("tx");
        let delta = sync_dirty_to_sqlite(&tx, &sync_snap, &mut fp, true).expect("sync");
        tx.commit().expect("commit");
        assert_eq!(delta.full_snapshot.is_some(), step == 2);
    }
    sync_snap.market_orders.get_mut(&700_100).unwrap().price = IskCents(777);
    {
        let tx = conn.transaction().expect("tx");
        sync_dirty_to_sqlite(&tx, &sync_snap, &mut fp, false).expect("same-len order");
        tx.commit().expect("commit");
    }
    let (loaded, _) = load_snapshot_from_sqlite(&conn).expect("load");
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
        let d = sync_dirty_to_sqlite(&tx, &sync_snap, &mut fp, true).expect("append 2 journal");
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
        let d = sync_dirty_to_sqlite(&tx, &sync_snap, &mut fp, true).expect("append 2 mails");
        tx.commit().expect("commit");
        assert!(d.full_snapshot.is_some());
    }
    let (loaded4, _) = load_snapshot_from_sqlite(&conn).expect("load 4");
    assert_eq!(loaded4.wallet_journal.len(), 4);
    assert_eq!(loaded4.wallet_journal[3].entry_id, 800_203);
    assert_eq!(loaded4.mails.len(), 4);
    assert_eq!(loaded4.mails[3].message_id, 630_203);

    // Clear wallet_journal & mails to empty (`len=0 < fp_len` with `fp_len=4`)
    sync_snap.wallet_journal.clear();
    sync_snap.mails.clear();
    {
        let tx = conn.transaction().expect("tx");
        let d = sync_dirty_to_sqlite(&tx, &sync_snap, &mut fp, true).expect("clear");
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
        let d = sync_dirty_to_sqlite(&tx, &sync_snap, &mut fp, true).expect("cold delta");
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
    let h0 = fp_asteroid(&base);
    assert_ne!(fp_asteroid(&AsteroidRecord { asteroid_id: 70_000_002, ..base }), h0);
    assert_ne!(fp_asteroid(&AsteroidRecord { solar_system_id: 30_000_143, ..base }), h0);
    assert_ne!(fp_asteroid(&AsteroidRecord { belt_id: 40_009_083, ..base }), h0);
    assert_ne!(fp_asteroid(&AsteroidRecord { type_id: 1228, ..base }), h0);
    assert_ne!(fp_asteroid(&AsteroidRecord { quantity: 9_999, ..base }), h0);
    assert_ne!(fp_asteroid(&AsteroidRecord { initial_quantity: 15_001, ..base }), h0);
    assert_ne!(fp_asteroid(&AsteroidRecord { position: (1.6, -2.5, 3.5), ..base }), h0);
    assert_ne!(fp_asteroid(&AsteroidRecord { position: (1.5, -2.6, 3.5), ..base }), h0);
    assert_ne!(fp_asteroid(&AsteroidRecord { position: (1.5, -2.5, 3.6), ..base }), h0);
    assert_ne!(fp_asteroid(&AsteroidRecord { radius: 125.5, ..base }), h0);

    let mut snap = build_populated_snapshot();
    let asteroid_id = base.asteroid_id;
    snap.asteroids.insert(asteroid_id, base);
    let fp = compute_fingerprints(&snap);
    assert_eq!(fp.asteroids[&asteroid_id], h0);
}

#[test]
fn canonical_fingerprints_distinguish_none_from_zero_sentinel_and_extra_fields() {
    use super::fingerprints::{
        fp_account, fp_json, fp_skill_queue, fp_structure,
    };

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
    assert_ne!(fp_account(&acc_none), fp_account(&acc_zero));

    let snap = build_populated_snapshot();
    let item_base = snap.items.values().next().unwrap().clone();
    let item_muta0 = InventoryItemRecord {
        source_mutaplasmid_id: Some(0),
        ..item_base.clone()
    };
    assert_ne!(fp_item(&InventoryItemRecord { source_mutaplasmid_id: None, ..item_base.clone() }), fp_item(&item_muta0));

    let item_bpc_none = InventoryItemRecord {
        blueprint_runs_remaining: None,
        ..item_base.clone()
    };
    let item_bpc_neg1 = InventoryItemRecord {
        blueprint_runs_remaining: Some(-1),
        ..item_base.clone()
    };
    assert_ne!(fp_item(&item_bpc_none), fp_item(&item_bpc_neg1));

    let c_base = snap.contracts.values().next().unwrap().clone();
    assert_ne!(
        fp_contract(&ContractRecord { acceptor_id: None, ..c_base.clone() }),
        fp_contract(&ContractRecord { acceptor_id: Some(-1), ..c_base.clone() })
    );
    assert_ne!(
        fp_contract(&c_base),
        fp_contract(&ContractRecord {
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
        fp_structure(&UpwellStructureRecord {
            self_destruct_at_unix: None,
            ..st_base
        }),
        fp_structure(&st_zero)
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
    assert_ne!(fp_skill_queue(&sq_none), fp_skill_queue(&sq_zero));

    let mut c1 = CorporationRecord::new(98_001, "Alpha", "ALP", 90_001);
    let mut c2 = CorporationRecord::new(98_001, "Alpha", "ALP", 90_001);
    for id in [10, 20, 30, 40, 50] {
        c1.members.insert(id);
    }
    for id in [50, 30, 10, 40, 20] {
        c2.members.insert(id);
    }
    assert_eq!(fp_json(&c1), fp_json(&c2));
}
