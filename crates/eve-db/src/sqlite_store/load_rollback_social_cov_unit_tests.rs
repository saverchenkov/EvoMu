//! Error-path and column-type mismatch tests for `load`, `rollback`, and `social_store`.

#![forbid(unsafe_code)]

use super::evedb_sqlite_cov_tests::build_populated_snapshot;
use super::evedb_sqlite_load_errors_and_schema_tests::open_migrated_conn;
use crate::sqlite_store::compute_fingerprints;
use crate::sqlite_store::load::{load_snapshot_from_sqlite, load_snapshot_from_sqlite_with_options};
use crate::sqlite_store::social_store::{load_d6_social_into_snapshot, sync_d6_social};
use crate::types::WorldStateSnapshot;
use rusqlite::params;

#[test]
fn load_rejects_wrong_sqlite_column_types_across_relational_tables() {
    // SQLite's dynamic typing allows inserting TEXT into non-PK INTEGER/REAL columns
    // or NULL into TEXT columns, exercising `r.get(...)?` error arms in `load.rs` and `social_store.rs`.
    let bad_rows = [
        "INSERT OR REPLACE INTO world_meta (meta_key, meta_value) VALUES ('wal_seq', 'bad_int')",
        "INSERT INTO accounts (username, user_id, password_token, role) VALUES ('u', 'bad_int', 'pw', 0)",
        "INSERT INTO characters (character_id, user_id, name, corporation_id, solar_system_id, active_ship_id, balance_cents, version, record_json) VALUES (1, 1, 'n', 1, 30000142, 1, 0, 1, X'FF')",
        "INSERT INTO skills (character_id, type_id, level, skill_points) VALUES (1, 'bad_int', 1, 0.0)",
        "INSERT INTO skill_queues (character_id, queue_index, type_id, target_level, start_sp, end_sp, duration_seconds) VALUES (1, 0, 'bad_int', 1, 0.0, 0.0, 0.0)",
        "INSERT INTO jump_clones (character_id, clones_json) VALUES (1, X'FF')",
        "INSERT INTO active_implants (character_id, implants_json) VALUES (1, X'FF')",
        "INSERT INTO items (item_id, type_id, owner_id, location_id, flag, quantity, singleton, version, record_json) VALUES (1, 1, 1, 1, 1, 1, 0, 1, X'FF')",
        "INSERT INTO saved_fittings (fitting_id, owner_id, name, ship_type_id, description, record_json) VALUES (1, 1, 'f', 1, '', X'FF')",
        "INSERT INTO market_orders (order_id, owner_id, station_id, region_id, type_id, is_bid, price_cents, vol_entered, vol_remaining, min_volume, is_npc, escrow_cents) VALUES (1, 'bad_int', 1, 1, 1, 0, 0, 1, 1, 1, 0, 0)",
        "INSERT INTO wallet_journal (seq_idx, entry_id, character_id, entry_type, amount_cents, balance_after_cents, reason) VALUES (1, 'bad_int', 1, 't', 0, 0, 'r')",
        "INSERT INTO contracts (contract_id, issuer_id, acceptor_id, contract_type, status, start_station_id, end_station_id, price_cents, reward_cents, collateral_cents, expiration_unix, record_json) VALUES (1, 1, NULL, 'c', 's', 1, 1, 0, 0, 0, 0, X'FF')",
        "INSERT INTO industry_jobs (job_id, installer_id, facility_id, blueprint_item_id, blueprint_type_id, activity_id, runs, product_type_id, status, duration_seconds, start_unix) VALUES (1, 'bad_int', 1, 1, 1, 1, 1, 1, 'Active', 1, 1)",
        "INSERT INTO planetary_colonies (owner_id, planet_id, solar_system_id, command_center_level, pins_count, last_extracted_unix, record_json) VALUES (1, 1, 30000142, 1, 0, 0, X'FF')",
        "INSERT INTO missions (character_id, mission_id, agent_id, solar_system_id, state, current_room_id, npcs_remaining, record_json) VALUES (1, 1, 1, 30000142, 'O', 0, 0, X'FF')",
        "INSERT INTO mission_tally (character_id, tally) VALUES (1, 'bad_int')",
        "INSERT INTO standings (character_id, tx_json) VALUES (1, X'FF')",
        "INSERT INTO npe_progress (character_id, node_graph_id, current_node_id, completed, record_json) VALUES (1, 1, 1, 0, X'FF')",
        "INSERT INTO bookmarks (bookmark_id, owner_id, label, solar_system_id, x, y, z, item_id, memo, folder_id, is_shared) VALUES (1, 'bad_int', 'b', 30000142, 0.0, 0.0, 0.0, NULL, '', NULL, 0)",
        "INSERT INTO mail (seq_idx, message_id, sender_id, recipient_id, subject, body) VALUES (1, 'bad_int', 1, 2, 's', 'b')",
        "INSERT INTO fleets (fleet_id, boss_id, free_move, is_registered, record_json) VALUES (1, 1, 0, 0, X'FF')",
        "INSERT INTO corps (corporation_id, name, ticker, ceo_id, alliance_id, tax_rate, master_wallet_cents, record_json) VALUES (1, 'c', 'C', 1, NULL, 0.0, 0, X'FF')",
        "INSERT INTO alliances (alliance_id, name, short_name, executor_corp_id, record_json) VALUES (1, 'a', 'A', 1, X'FF')",
        "INSERT INTO wars (war_id, aggressor_corp_id, defender_corp_id, mutual, started_unix, active) VALUES (1, 'bad_int', 2, 0, 0, 1)",
        "INSERT INTO corp_projects (project_id, corporation_id, name, target_quantity, current_quantity, isk_per_unit_cents, escrow_remaining_cents) VALUES (1, 'bad_int', 'p', 1, 0, 0, 0)",
        "INSERT INTO killmails (kill_id, victim_character_id, attacker_character_id, solar_system_id, ship_type_id, total_value_isk) VALUES (1, 'bad_int', 2, 30000142, 587, 0.0)",
        "INSERT INTO kill_rights (kill_right_id, owner_id, target_id, activation_fee_cents, is_active_suspect) VALUES (1, 'bad_int', 2, 0, 0)",
        "INSERT INTO structures (structure_id, type_id, owner_corp_id, solar_system_id, name, state, quantum_core_installed, fuel_blocks, shield_hp, armor_hp, hull_hp, record_json) VALUES (1, 35832, 1, 30000142, 's', 'ShieldVulnerable', 0, 0, 1.0, 1.0, 1.0, X'FF')",
        "INSERT INTO system_states (solar_system_id, dbs_multiplier, ess_main_bank_cents, ess_reserve_bank_cents, sov_alliance_id, record_json) VALUES (30000142, 1.0, 0, 0, NULL, X'FF')",
        "INSERT INTO idempotency_keys (character_id, idem_key) VALUES ('bad_int', 'k')",
        "INSERT INTO licensed_skins (character_id, skins_json) VALUES (1, X'FF')",
        "INSERT INTO applied_ship_skins (ship_id, skin_id) VALUES (1, 'bad_int')",
        // social_store tables:
        "INSERT INTO read_mails (character_id, mail_id) VALUES ('bad_int', 1)",
        "INSERT INTO deleted_mails (character_id, mail_id) VALUES ('bad_int', 1)",
        "INSERT INTO trashed_mails (character_id, mail_id) VALUES ('bad_int', 1)",
        "INSERT INTO mail_labels (character_id, labels_json) VALUES (1, X'FF')",
        "INSERT INTO mail_assigned_labels (character_id, mail_id, labels_json) VALUES ('bad_int', 1, '[]')",
        "INSERT INTO mailing_lists (list_id, record_json) VALUES (1, X'FF')",
        "INSERT INTO calendar_events (event_id, record_json) VALUES (1, X'FF')",
        "INSERT INTO bookmark_meta (bookmark_id, record_json) VALUES (1, X'FF')",
        "INSERT INTO bookmark_folders (folder_id, record_json) VALUES (1, X'FF')",
        "INSERT INTO access_groups (group_id, members_json) VALUES (1, X'FF')",
        "INSERT INTO chat_channels (channel_id, record_json) VALUES ('ch1', X'FF')",
        "INSERT INTO char_contacts (character_id, contacts_json) VALUES (1, X'FF')",
        "INSERT INTO char_contact_notifications (character_id, notifications_json) VALUES (1, X'FF')",
        "INSERT INTO char_contact_labels (character_id, labels_json) VALUES (1, X'FF')",
        "INSERT INTO corp_contacts (corporation_id, contacts_json) VALUES (1, X'FF')",
        "INSERT INTO corp_contact_labels (corporation_id, labels_json) VALUES (1, X'FF')",
        "INSERT INTO alliance_contacts (alliance_id, contacts_json) VALUES (1, X'FF')",
        "INSERT INTO alliance_contact_labels (alliance_id, labels_json) VALUES (1, X'FF')",
        "INSERT INTO char_notes (character_id, item_id, note) VALUES ('bad_int', 1, 'note')",
    ];

    for sql in bad_rows {
        let conn = open_migrated_conn();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute(sql, [])
            .unwrap_or_else(|e| panic!("insert bad row `{sql}`: {e}"));
        assert!(
            load_snapshot_from_sqlite(&conn).is_err(),
            "expected Err for bad row `{sql}`"
        );
    }

    // Wrong column types on graceful extension tables in load.rs (exercises `if let Ok(r)` fallbacks)
    let graceful_bad = [
        "INSERT INTO char_mining_ledger (character_id, solar_system_id, type_id, quantity, date, timestamp_unix) VALUES (1, 'bad', 34, 1, '2026-01-01', 1)",
        "INSERT INTO asteroids (asteroid_id, solar_system_id, belt_id, type_id, quantity, initial_quantity, x, y, z, radius) VALUES (1, 'bad', 1, 1, 1, 1, 0.0, 0.0, 0.0, 1.0)",
        "INSERT INTO character_last_jump_unix (character_id, last_jump_unix) VALUES (1, 'bad')",
        "INSERT INTO character_cspa_charges (character_id, cspa_charge_cents) VALUES (1, 'bad')",
        "INSERT INTO mail_timestamps (mail_id, timestamp_unix) VALUES (1, 'bad')",
        "INSERT INTO character_login_stats (character_id, last_login_unix, last_logout_unix, logins_count) VALUES (1, 'bad', 0, 0)",
        "INSERT INTO item_custom_names (item_id, custom_name) VALUES (1, X'FF')",
        "INSERT INTO item_positions (item_id, x, y, z) VALUES (1, 'bad', 0.0, 0.0)",
        "INSERT INTO structure_vulnerability_windows (structure_id, window_start_unix, window_end_unix) VALUES (1, 'bad', 0)",
        "INSERT INTO character_doll_info (character_id, doll_dna, portrait_meta) VALUES (1, X'FF', X'FF')",
    ];
    let conn = open_migrated_conn();
    for sql in graceful_bad {
        conn.execute(sql, [])
            .unwrap_or_else(|e| panic!("insert graceful_bad `{sql}`: {e}"));
    }
    assert!(
        load_snapshot_from_sqlite(&conn).is_err(),
        "corrupt extension rows must fail closed by default (#906)"
    );
    let (loaded, _) =
        crate::sqlite_store::load_snapshot_from_sqlite_with_options(&conn, true).unwrap();
    assert!(loaded.asteroids.is_empty());
    assert!(loaded.item_custom_names.is_empty());
}

#[test]
fn social_store_sync_and_load_reject_every_dropped_table() {
    let social_tables = [
        "read_mails",
        "deleted_mails",
        "trashed_mails",
        "mail_labels",
        "mail_assigned_labels",
        "mailing_lists",
        "calendar_events",
        "bookmark_meta",
        "bookmark_folders",
        "access_groups",
        "chat_channels",
        "char_contacts",
        "char_contact_notifications",
        "char_contact_labels",
        "corp_contacts",
        "corp_contact_labels",
        "alliance_contacts",
        "alliance_contact_labels",
        "char_notes",
    ];

    let mut snap = build_populated_snapshot();
    let cid = 90_000_001_i64;
    snap.read_mails.insert((cid, 1));
    snap.deleted_mails.insert((cid, 2));
    snap.trashed_mails.insert((cid, 3));
    snap.mail_labels.insert(
        cid,
        vec![crate::types::MailLabelRecord {
            label_id: 1,
            name: "L".into(),
            color: "#000000".into(),
        }],
    );
    snap.mail_assigned_labels.insert((cid, 1), vec![1]);
    snap.mailing_lists.insert(
        10,
        crate::types::MailingListRecord {
            list_id: 10,
            name: "ML".into(),
            owner_id: cid,
            default_access: 1,
            default_role: 0,
            cost: 0,
            members: std::collections::HashMap::from([(cid, 1)]),
            entity_access: std::collections::HashMap::new(),
            welcome_mail: None,
            mail_ids: std::collections::HashSet::new(),
        },
    );
    snap.calendar_events.insert(
        20,
        crate::types::CalendarEventRecord {
            event_id: 20,
            owner_id: cid,
            creator_id: cid,
            event_date_time: 100,
            duration: 30,
            importance: 0,
            title: "Ev".into(),
            description: "D".into(),
            scope: "Personal".into(),
            responses: std::collections::HashMap::new(),
            is_deleted: false,
        },
    );
    snap.bookmark_subfolders.insert(30, 40);
    snap.bookmark_folders.insert(
        40,
        crate::types::BookmarkFolderRecord {
            folder_id: 40,
            owner_id: cid,
            folder_name: "F".into(),
            ..Default::default()
        },
    );
    snap.access_groups.insert(50, vec![cid]);
    snap.chat_channels.insert(
        "ch1".into(),
        crate::types::ChatChannelRecord {
            channel_id: "ch1".into(),
            name: "Ch".into(),
            motd: String::new(),
            owner_id: Some(cid),
            members: std::collections::HashSet::from([cid]),
            is_system_local: false,
            solar_system_id: None,
            is_private: false,
            operators: std::collections::HashSet::new(),
            password: None,
            is_delayed_local: false,
        },
    );
    let contact = crate::types::SocialContactRecord {
        contact_id: 90_000_002,
        contact_type: "character".into(),
        standing: 1.0,
        is_watched: false,
        is_blocked: false,
        label_ids: vec![],
    };
    snap.char_contacts.insert(cid, vec![contact.clone()]);
    snap.char_contact_notifications.insert(
        cid,
        vec![crate::durable_domain_types::SocialContactNotificationRecord {
            notification_id: 1,
            sender_character_id: 90_000_002,
            send_date_unix: 100,
            standing_level: 1.0,
            message: "Hi".into(),
        }],
    );
    snap.char_contact_labels.insert(cid, vec![(1, "F".into())]);
    snap.corp_contacts.insert(98_000_001, vec![contact.clone()]);
    snap.corp_contact_labels
        .insert(98_000_001, vec![(1, "C".into(), 1)]);
    snap.alliance_contacts.insert(99_000_001, vec![contact]);
    snap.alliance_contact_labels
        .insert(99_000_001, vec![(1, "A".into())]);
    snap.char_notes.insert((cid, 90_000_002), "note".into());
    for tbl in social_tables {
        let mut conn = open_migrated_conn();
        conn.execute(&format!("DROP TABLE {tbl}"), []).unwrap();
        let mut dst = WorldStateSnapshot::default();
        assert!(
            load_d6_social_into_snapshot(&conn, &mut dst).is_err(),
            "expected load_d6_social Err when {tbl} is dropped"
        );
        let mut fp = compute_fingerprints(&WorldStateSnapshot::default());
        let tx = conn.transaction().unwrap();
        assert!(
            sync_d6_social(&tx, &snap, &mut fp).is_err(),
            "expected sync_d6_social Err when {tbl} is dropped"
        );
    }
}

#[test]
fn load_rejects_missing_tables_and_corrupt_rows() {
    let snap = build_populated_snapshot();

    // 1. Missing tables when loading SQLite snapshot
    let required_tables = [
        "accounts",
        "characters",
        "items",
        "market_orders",
        "wallet_journal",
        "mail",
        "skills",
        "skill_queues",
        "jump_clones",
        "active_implants",
        "saved_fittings",
        "contracts",
        "industry_jobs",
        "planetary_colonies",
        "missions",
        "standings",
        "npe_progress",
        "bookmarks",
        "fleets",
        "corps",
        "alliances",
        "wars",
        "corp_projects",
        "killmails",
        "kill_rights",
        "structures",
        "system_states",
        "licensed_skins",
    ];

    for tbl in &required_tables {
        let mut conn = open_migrated_conn();
        crate::sqlite_store::sync::replace_sqlite_from_snapshot(&mut conn, &snap).unwrap();
        conn.execute(&format!("DROP TABLE {tbl}"), []).unwrap();
        assert!(
            load_snapshot_from_sqlite(&conn).is_err(),
            "expected load Err when table {tbl} is dropped"
        );
    }

    // 2. Corrupt JSON on characters / items / cold JSON tables during load
    let corrupt_load_sql = [
        "UPDATE characters SET record_json = '{bad'",
        "UPDATE items SET record_json = '{bad'",
        "UPDATE jump_clones SET clones_json = '{bad'",
        "UPDATE active_implants SET implants_json = '{bad'",
        "UPDATE saved_fittings SET record_json = '{bad'",
        "UPDATE contracts SET record_json = '{bad'",
        "UPDATE planetary_colonies SET record_json = '{bad'",
        "UPDATE missions SET record_json = '{bad'",
        "UPDATE standings SET tx_json = '{bad'",
        "UPDATE npe_progress SET record_json = '{bad'",
        "UPDATE fleets SET record_json = '{bad'",
        "UPDATE corps SET record_json = '{bad'",
        "UPDATE alliances SET record_json = '{bad'",
        "UPDATE structures SET record_json = '{bad'",
        "UPDATE system_states SET record_json = '{bad'",
        "UPDATE licensed_skins SET skins_json = '{bad'",
    ];
    for sql in corrupt_load_sql {
        let mut conn = open_migrated_conn();
        crate::sqlite_store::sync::replace_sqlite_from_snapshot(&mut conn, &snap).unwrap();
        conn.execute(sql, []).unwrap();
        assert!(
            load_snapshot_from_sqlite(&conn).is_err(),
            "expected load Err for `{sql}`"
        );
    }

    // 3. Corrupt optional extension tables during load (quarantine mode skips corrupt rows)
    let mut conn = open_migrated_conn();
    crate::sqlite_store::sync::replace_sqlite_from_snapshot(&mut conn, &snap).unwrap();
    conn.execute("UPDATE asteroids SET solar_system_id = 'bad'", [])
        .unwrap();
    conn.execute("UPDATE item_custom_names SET custom_name = X'FF'", [])
        .unwrap();
    assert!(load_snapshot_from_sqlite_with_options(&conn, true).is_ok());

    let _ = params![1i64];
}
