//! Regression tests for Round-6 Quasar persistence bugs:
//! - `#410` (`CharacterRecord` profile/autopilot fields persisted & rolled back via `fp_character`)
//! - `#411` (`WorldStateSnapshot` D6 batch4 auxiliary collections synced, loaded, and rolled back)
//! - `#412` (`rollback_snapshot` preserving `bookmarks` extended fields & same-length `wallet_journal`/`mails`)
//! - `#403` (Unified account PLEX Vault ledger & SQLite round-trip)

#![forbid(unsafe_code)]

use crate::persistence::WorldRepository;
use crate::sqlite_store::fingerprints::{fp_character, TableFingerprints};
use crate::sqlite_store::load::load_snapshot_from_sqlite;
use crate::sqlite_store::schema::apply_migrations;
use crate::sqlite_store::sync::sync_dirty_to_sqlite;
use crate::types::{BookmarkRecord, MailRecord, WalletJournalEntry, WorldStateSnapshot};
use crate::{
    CorpProjectContributorEntry, IskCents, KillmailAttackerDetail, KillmailDetailRecord,
    LoyaltyStoreOfferItemRecord, LoyaltyStoreOfferRecord, StructureEsiTimerMeta, SystemJumpEvent,
    SystemKillEvent,
};
use rusqlite::Connection;

fn open_migrated_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory sqlite");
    apply_migrations(&conn).expect("run migrations");
    conn
}

fn sync_conn(conn: &mut Connection, snap: &WorldStateSnapshot, fp: &mut TableFingerprints) {
    let tx = conn.transaction().expect("begin sqlite tx");
    sync_dirty_to_sqlite(&tx, snap, fp, false).expect("sync dirty snapshot");
    tx.commit().expect("commit sqlite tx");
}

#[test]
fn regression_410_character_profile_and_autopilot_fields_dirty_detect_and_roundtrip() {
    let mut conn = open_migrated_conn();
    let mut snap = WorldRepository::create_demo_snapshot();
    let cid = 90_000_001i64;
    let mut fp = TableFingerprints::default();
    sync_conn(&mut conn, &snap, &mut fp);

    let base_fp = fp_character(&snap.characters[&cid]);
    let mut mutated = snap.characters[&cid].clone();
    mutated.description = "Updated bio for #410".into();
    assert_ne!(fp_character(&mutated), base_fp);

    mutated = snap.characters[&cid].clone();
    mutated.title = "Wing Commander".into();
    assert_ne!(fp_character(&mutated), base_fp);

    mutated = snap.characters[&cid].clone();
    mutated.gender = 2;
    assert_ne!(fp_character(&mutated), base_fp);

    mutated = snap.characters[&cid].clone();
    mutated.create_date_unix = 1_750_000_123;
    assert_ne!(fp_character(&mutated), base_fp);

    mutated = snap.characters[&cid].clone();
    mutated.autopilot_waypoints = vec![30_000_142, 30_002_187, 30_000_001];
    assert_ne!(fp_character(&mutated), base_fp);

    {
        let ch = snap.characters.get_mut(&cid).unwrap();
        ch.description = "Persistent Capsuleer Bio".into();
        ch.title = "Fleet Executor".into();
        ch.gender = 2;
        ch.create_date_unix = 1_760_111_222;
        ch.autopilot_waypoints = vec![30_000_142, 30_003_458];
    }
    sync_conn(&mut conn, &snap, &mut fp);
    let (loaded, _) = load_snapshot_from_sqlite(&conn).expect("load snapshot");
    let ch_loaded = &loaded.characters[&cid];
    assert_eq!(ch_loaded.description, "Persistent Capsuleer Bio");
    assert_eq!(ch_loaded.title, "Fleet Executor");
    assert_eq!(ch_loaded.gender, 2);
    assert_eq!(ch_loaded.create_date_unix, 1_760_111_222);
    assert_eq!(ch_loaded.autopilot_waypoints, vec![30_000_142, 30_003_458]);

    snap.characters.get_mut(&cid).unwrap().autopilot_waypoints = vec![30_000_999];
    let (reloaded_abort, _) = load_snapshot_from_sqlite(&conn).expect("reload character");
    assert_eq!(
        reloaded_abort.characters[&cid].autopilot_waypoints,
        vec![30_000_142, 30_003_458]
    );
}

#[test]
fn regression_411_world_snapshot_auxiliary_collections_persist_and_rollback() {
    let mut conn = open_migrated_conn();
    let mut snap = WorldRepository::create_demo_snapshot();

    snap.killmail_details.insert(
        77_001,
        KillmailDetailRecord {
            kill_id: 77_001,
            killmail_time_unix: 1_760_000_500,
            damage_taken: 14_250,
            victim_position: Some([1200.5, -3400.25, 9876.0]),
            attackers: vec![KillmailAttackerDetail {
                character_id: 90_000_002,
                corporation_id: 1_000_045,
                alliance_id: None,
                ship_type_id: 24_690,
                weapon_type_id: 2_185,
                damage_done: 14_250,
                final_blow: true,
                security_status: -1.4,
            }],
        },
    );
    snap.station_office_rental_costs.insert(60_003_760, 125_000.0);
    snap.structure_esi_timer_meta.insert(
        1_020_000_000_001,
        StructureEsiTimerMeta {
            profile_id: 12,
            fuel_expires_unix: Some(1_765_000_000),
            state_timer_start_unix: Some(1_760_000_100),
            state_timer_end_unix: Some(1_760_086_500),
            unanchors_at_unix: None,
            next_reinforce_hour: Some(19),
            next_reinforce_apply_unix: Some(1_761_000_000),
        },
    );
    snap.corp_project_contributors.insert(
        501,
        vec![CorpProjectContributorEntry {
            character_id: 90_000_001,
            contributed_quantity: 250,
        }],
    );
    snap.loyalty_store_offers.insert(
        1_000_035,
        vec![LoyaltyStoreOfferRecord {
            offer_id: 9001,
            type_id: 17_703,
            quantity: 1,
            lp_cost: 40_000,
            isk_cost: 30_000_000,
            ak_cost: None,
            required_items: vec![LoyaltyStoreOfferItemRecord {
                type_id: 34,
                quantity: 10_000,
            }],
        }],
    );
    snap.system_jump_events.push(SystemJumpEvent {
        solar_system_id: 30_000_142,
        character_id: 90_000_001,
        timestamp_unix: 1_760_000_700,
    });
    snap.system_kill_events.push(SystemKillEvent {
        solar_system_id: 30_000_142,
        ship_kills: 4,
        npc_kills: 19,
        pod_kills: 2,
        timestamp_unix: 1_760_000_750,
    });

    let mut fp = TableFingerprints::default();
    sync_conn(&mut conn, &snap, &mut fp);

    let (loaded, _) = load_snapshot_from_sqlite(&conn).expect("load #411 snapshot");
    assert_eq!(loaded.killmail_details, snap.killmail_details);
    assert_eq!(loaded.station_office_rental_costs, snap.station_office_rental_costs);
    assert_eq!(loaded.structure_esi_timer_meta, snap.structure_esi_timer_meta);
    assert_eq!(loaded.corp_project_contributors, snap.corp_project_contributors);
    assert_eq!(loaded.loyalty_store_offers, snap.loyalty_store_offers);
    assert_eq!(loaded.system_jump_events, snap.system_jump_events);
    assert_eq!(loaded.system_kill_events, snap.system_kill_events);

    let mut working = loaded.clone();
    working.killmail_details.clear();
    working.station_office_rental_costs.insert(60_003_760, 999.0);
    working.structure_esi_timer_meta.clear();
    working.corp_project_contributors.clear();
    working.loyalty_store_offers.clear();
    working.system_jump_events.clear();
    working.system_kill_events.clear();

    let (reloaded, _) = load_snapshot_from_sqlite(&conn).expect("reload #411 collections");
    assert_eq!(reloaded.killmail_details, loaded.killmail_details);
    assert_eq!(reloaded.station_office_rental_costs, loaded.station_office_rental_costs);
    assert_eq!(reloaded.structure_esi_timer_meta, loaded.structure_esi_timer_meta);
    assert_eq!(reloaded.corp_project_contributors, loaded.corp_project_contributors);
    assert_eq!(reloaded.loyalty_store_offers, loaded.loyalty_store_offers);
    assert_eq!(reloaded.system_jump_events, loaded.system_jump_events);
    assert_eq!(reloaded.system_kill_events, loaded.system_kill_events);
}

#[test]
fn regression_412_rollback_preserves_bookmarks_metadata_and_same_length_journal_mail() {
    let mut conn = open_migrated_conn();
    let mut snap = WorldRepository::create_demo_snapshot();
    snap.bookmarks.insert(
        88_001,
        BookmarkRecord {
            bookmark_id: 88_001,
            owner_id: 90_000_001,
            label: "Jita Instadock".into(),
            solar_system_id: 30_000_142,
            x: 100.0,
            y: 200.0,
            z: 300.0,
            item_id: Some(60_003_760),
            memo: "Align to Jita 4-4 CNAP".into(),
            folder_id: Some(42),
            is_shared: true,
        },
    );
    snap.wallet_journal.push(WalletJournalEntry {
        entry_id: 99_001,
        character_id: 90_000_001,
        entry_type: "bounty_prizes".into(),
        amount: IskCents::from_isk(2_500_000),
        balance_after: IskCents::from_isk(1_002_500_000),
        reason: "Serpentis bounty".into(),
    });
    snap.mails.push(MailRecord {
        message_id: 99_002,
        sender_id: 90_000_002,
        recipient_id: 90_000_001,
        subject: "Fleet Ping".into(),
        body: "Undock in 5 minutes".into(),
    });

    let mut fp = TableFingerprints::default();
    sync_conn(&mut conn, &snap, &mut fp);

    let bm = snap.bookmarks.get_mut(&88_001).unwrap();
    bm.label = "Corrupted".into();
    bm.item_id = None;
    bm.memo = String::new();
    bm.folder_id = None;
    bm.is_shared = false;

    let j_len = snap.wallet_journal.len();
    snap.wallet_journal[j_len - 1].reason = "Tampered same-length journal".into();
    snap.wallet_journal[j_len - 1].amount = IskCents::from_isk(-999_999);
    let m_len = snap.mails.len();
    snap.mails[m_len - 1].subject = "Tampered same-length subject".into();
    snap.mails[m_len - 1].body = "Tampered same-length body".into();

    let (restored, _) = load_snapshot_from_sqlite(&conn).expect("reload #412 mutations");
    let _ = snap;

    let restored_bm = &restored.bookmarks[&88_001];
    assert_eq!(restored_bm.label, "Jita Instadock");
    assert_eq!(restored_bm.item_id, Some(60_003_760));
    assert_eq!(restored_bm.memo, "Align to Jita 4-4 CNAP");
    assert_eq!(restored_bm.folder_id, Some(42));
    assert!(restored_bm.is_shared);

    assert_eq!(restored.wallet_journal[j_len - 1].reason, "Serpentis bounty");
    assert_eq!(
        restored.wallet_journal[j_len - 1].amount,
        IskCents::from_isk(2_500_000)
    );
    assert_eq!(restored.mails[m_len - 1].subject, "Fleet Ping");
    assert_eq!(restored.mails[m_len - 1].body, "Undock in 5 minutes");
}

#[test]
fn regression_403_unified_plex_vault_ledger_and_sqlite_roundtrip() {
    let mut conn = open_migrated_conn();
    let mut snap = WorldRepository::create_demo_snapshot();
    let cid = 90_000_001i64;
    let uid = snap.account_of_character(cid).expect("account for demo char");

    assert_eq!(snap.unified_plex_balance_for_user(uid), 500);
    assert_eq!(snap.unified_plex_balance(cid), 500);

    snap.characters.get_mut(&cid).unwrap().plex_balance = 125;
    assert_eq!(snap.unified_plex_balance_for_user(uid), 625);
    assert_eq!(snap.adjust_unified_plex(cid, -225), Some(400));
    assert_eq!(snap.account_plex_vault[&uid], 400);
    assert_eq!(snap.characters[&cid].plex_balance, 0);

    assert_eq!(snap.adjust_unified_plex_for_user(uid, -401), None);
    assert_eq!(snap.unified_plex_balance_for_user(uid), 400);

    let mut fp = TableFingerprints::default();
    sync_conn(&mut conn, &snap, &mut fp);
    let (loaded, _) = load_snapshot_from_sqlite(&conn).expect("load unified plex vault");
    assert_eq!(loaded.unified_plex_balance_for_user(uid), 400);
    assert_eq!(loaded.unified_plex_balance(cid), 400);
    assert_eq!(loaded.characters[&cid].plex_balance, 0);
}
