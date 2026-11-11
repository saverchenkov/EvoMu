//! Error-path and edge-case branch coverage tests for `load`, `rollback`,
//! `social_store`, `ledger_store`, `notes_lp_sequence_store`, `corp_crimewatch_store`,
//! `industry_market_pve_store`, and `schema`.

#![forbid(unsafe_code)]

use super::evedb_sqlite_cov_tests::build_populated_snapshot;
use crate::sqlite_store::corp_crimewatch_store::load_d6_corp_crimewatch;
use crate::sqlite_store::fingerprints::compute_fingerprints;
use crate::sqlite_store::industry_market_pve_store::load_d6_batch4;
use crate::sqlite_store::ledger_store::{load_ledger, sync_ledger};
use crate::sqlite_store::load::load_snapshot_from_sqlite;
use crate::sqlite_store::notes_lp_sequence_store::{load_d6, sync_d6};
use crate::sqlite_store::rollback::rollback_dirty_from_sqlite;
use crate::sqlite_store::schema::{apply_migrations, load_or_init_jwt_secret, save_jwt_secret};
use crate::sqlite_store::social_store::{load_d6_social_into_snapshot, sync_d6_social};
use crate::types::WorldStateSnapshot;
use rusqlite::{params, Connection};

pub(super) fn open_migrated_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory sqlite");
    apply_migrations(&conn).expect("apply migrations");
    conn
}

#[test]
fn sqlite_store_load_error_and_optional_table_missing_branches() {
    let strict_load_tables = [
        "world_meta",
        "accounts",
        "characters",
        "skills",
        "skill_queues",
        "jump_clones",
        "active_implants",
        "items",
        "saved_fittings",
        "market_orders",
        "wallet_journal",
        "contracts",
        "industry_jobs",
        "planetary_colonies",
        "missions",
        "mission_tally",
        "standings",
        "npe_progress",
        "bookmarks",
        "mail",
        "fleets",
        "corps",
        "alliances",
        "wars",
        "corp_projects",
        "killmails",
        "kill_rights",
        "structures",
        "system_states",
        "idempotency_keys",
        "licensed_skins",
        "applied_ship_skins",
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

    for tbl in strict_load_tables {
        let conn = open_migrated_conn();
        conn.execute(&format!("DROP TABLE {tbl}"), [])
            .unwrap_or_else(|e| panic!("drop {tbl}: {e}"));
        assert!(
            load_snapshot_from_sqlite(&conn).is_err(),
            "expected load_snapshot_from_sqlite error when strict table {tbl} is dropped"
        );
    }

    // Drop graceful extension tables together so every `if let Ok(mut stmt)` else-branch executes.
    let graceful_tables = [
        "char_mining_ledger",
        "asteroids",
        "character_last_jump_unix",
        "character_cspa_charges",
        "mail_timestamps",
        "character_login_stats",
        "item_custom_names",
        "item_positions",
        "structure_vulnerability_windows",
        "skill_history",
        "character_doll_info",
    ];
    let conn = open_migrated_conn();
    for tbl in graceful_tables {
        conn.execute(&format!("DROP TABLE {tbl}"), [])
            .unwrap_or_else(|e| panic!("drop {tbl}: {e}"));
    }
    let (loaded, _) = load_snapshot_from_sqlite(&conn)
        .expect("optional extension tables missing should still load base snapshot");
    assert!(loaded.asteroids.is_empty());
    assert!(loaded.character_doll_info.is_empty());
    assert!(loaded.skill_history.is_empty());
}

#[test]
fn sqlite_store_malformed_json_and_unknown_kv_keys_coverage() {
    // 1. Rollback malformed skill_history JSON + optional corrupt skill_history on load
    let snap = build_populated_snapshot();
    let fp = compute_fingerprints(&snap);
    {
        let conn = open_migrated_conn();
        conn.execute(
            "INSERT INTO skill_history (character_id, history_json) VALUES (?1, '{not-json')",
            params![90_000_001i64],
        )
        .expect("insert corrupt skill_history");
        let mut dirty = snap.clone();
        dirty.skill_history.get_mut(&90_000_001).unwrap()[0].skill_level = 99;
        rollback_dirty_from_sqlite(&conn, &mut dirty, &fp)
            .expect("rollback skips corrupt optional skill_history JSON cleanly");
        let err = load_snapshot_from_sqlite(&conn).expect_err("corrupt skill_history fails closed");
        assert!(err.contains("skill_history") && err.contains("90000001"));
        let (reloaded, _) =
            crate::sqlite_store::load_snapshot_from_sqlite_with_options(&conn, true)
                .expect("quarantine mode skips and moves corrupt skill_history");
        assert!(reloaded.skill_history.is_empty());
    }

    let corrupt_json_inserts = [
        "INSERT INTO characters (character_id, user_id, name, corporation_id, solar_system_id, active_ship_id, balance_cents, version, record_json) VALUES (1, 1, 'x', 1, 30000142, 1, 0, 1, '{bad')",
        "INSERT INTO jump_clones (character_id, clones_json) VALUES (1, '{bad')",
        "INSERT INTO active_implants (character_id, implants_json) VALUES (1, '{bad')",
        "INSERT INTO items (item_id, type_id, owner_id, location_id, flag, quantity, singleton, version, record_json) VALUES (1, 1, 1, 1, 1, 1, 0, 1, '{bad')",
        "INSERT INTO saved_fittings (fitting_id, owner_id, name, ship_type_id, description, record_json) VALUES (1, 1, 'f', 1, '', '{bad')",
        "INSERT INTO contracts (contract_id, issuer_id, acceptor_id, contract_type, status, start_station_id, end_station_id, price_cents, reward_cents, collateral_cents, expiration_unix, record_json) VALUES (1, 1, NULL, 'Courier', 'Outstanding', 60003760, 60003760, 0, 0, 0, 1, '{bad')",
        "INSERT INTO planetary_colonies (owner_id, planet_id, solar_system_id, command_center_level, pins_count, last_extracted_unix, record_json) VALUES (1, 1, 30000142, 1, 0, 1, '{bad')",
        "INSERT INTO missions (character_id, mission_id, agent_id, solar_system_id, state, current_room_id, npcs_remaining, record_json) VALUES (1, 1, 1, 30000142, 'Offered', 0, 0, '{bad')",
        "INSERT INTO npe_progress (character_id, node_graph_id, current_node_id, completed, record_json) VALUES (1, 1, 1, 0, '{bad')",
        "INSERT INTO fleets (fleet_id, boss_id, free_move, is_registered, record_json) VALUES (1, 1, 0, 0, '{bad')",
        "INSERT INTO corps (corporation_id, name, ticker, ceo_id, alliance_id, tax_rate, master_wallet_cents, record_json) VALUES (1, 'c', 'C', 1, NULL, 0.0, 0, '{bad')",
        "INSERT INTO alliances (alliance_id, name, short_name, executor_corp_id, record_json) VALUES (1, 'a', 'A', 1, '{bad')",
        "INSERT INTO structures (structure_id, type_id, owner_corp_id, solar_system_id, name, state, quantum_core_installed, fuel_blocks, shield_hp, armor_hp, hull_hp, record_json) VALUES (1, 35832, 1, 30000142, 's', 'ShieldVulnerable', 0, 0, 1.0, 1.0, 1.0, '{bad')",
        "INSERT INTO system_states (solar_system_id, dbs_multiplier, ess_main_bank_cents, ess_reserve_bank_cents, sov_alliance_id, record_json) VALUES (30000142, 1.0, 0, 0, NULL, '{bad')",
        "INSERT INTO licensed_skins (character_id, skins_json) VALUES (1, '{bad')",
        "INSERT INTO standings (character_id, tx_json) VALUES (1, '{bad')",
        "INSERT INTO mail_labels (character_id, labels_json) VALUES (1, '{bad')",
        "INSERT INTO mailing_lists (list_id, record_json) VALUES (1, '{bad')",
        "INSERT INTO calendar_events (event_id, record_json) VALUES (1, '{bad')",
        "INSERT INTO bookmark_meta (bookmark_id, record_json) VALUES (1, '{bad')",
        "INSERT INTO bookmark_folders (folder_id, record_json) VALUES (1, '{bad')",
        "INSERT INTO access_groups (group_id, members_json) VALUES (1, '{bad')",
        "INSERT INTO chat_channels (channel_id, record_json) VALUES ('ch1', '{bad')",
        "INSERT INTO char_contacts (character_id, contacts_json) VALUES (1, '{bad')",
        "INSERT INTO char_contact_notifications (character_id, notifications_json) VALUES (1, '{bad')",
        "INSERT INTO char_contact_labels (character_id, labels_json) VALUES (1, '{bad')",
        "INSERT INTO corp_contacts (corporation_id, contacts_json) VALUES (1, '{bad')",
        "INSERT INTO corp_contact_labels (corporation_id, labels_json) VALUES (1, '{bad')",
        "INSERT INTO alliance_contacts (alliance_id, contacts_json) VALUES (1, '{bad')",
        "INSERT INTO alliance_contact_labels (alliance_id, labels_json) VALUES (1, '{bad')",
    ];
    for sql in corrupt_json_inserts {
        let conn = open_migrated_conn();
        conn.execute_batch("PRAGMA foreign_keys = OFF;")
            .expect("disable fk for corrupt row insert");
        conn.execute(sql, [])
            .unwrap_or_else(|e| panic!("insert corrupt row ({sql}): {e}"));
        assert!(
            load_snapshot_from_sqlite(&conn).is_err(),
            "expected load error for corrupt JSON SQL: {sql}"
        );
    }

    // Unknown KV keys + corrupt JSON in ledger / D6 / crimewatch / batch4 stores
    let conn = open_migrated_conn();
    conn.execute(
        "INSERT OR REPLACE INTO world_meta (meta_key, meta_value) VALUES ('unknown_future_meta_key', 999)",
        [],
    )
    .expect("insert unknown world_meta key");
    conn.execute(
        "INSERT OR REPLACE INTO d6_corp_crimewatch_kv (store_key, payload_json) VALUES ('unknown_kv_key', '{}'), ('criminal_timers', '{bad')",
        [],
    )
    .expect("insert unknown and corrupt crimewatch kv");
    conn.execute(
        "INSERT OR REPLACE INTO world_kv_blobs (blob_key, blob_json) VALUES ('unknown_blob_key', '{}'), ('killmail_details', '{bad')",
        [],
    )
    .expect("insert unknown and corrupt world_kv_blobs");
    conn.execute(
        "INSERT INTO wallet_journal_meta (entry_id, meta_json) VALUES (999, '{bad')",
        [],
    )
    .expect("insert corrupt wallet_journal_meta");
    conn.execute(
        "INSERT INTO market_transactions (transaction_id, tx_json) VALUES (999, '{bad')",
        [],
    )
    .expect("insert corrupt market_transactions");
    conn.execute(
        "INSERT INTO char_lp_logs (character_id, logs_json) VALUES (999, '{bad')",
        [],
    )
    .expect("insert corrupt char_lp_logs");

    let mut dst = WorldStateSnapshot::default();
    load_d6(&conn, &mut dst);
    load_ledger(&conn, &mut dst);
    load_d6_corp_crimewatch(&conn, &mut dst);
    load_d6_batch4(&conn, &mut dst);
    assert!(dst.killmail_details.is_empty());

    // Dropping optional KV and ledger tables exercises every graceful load fallback branch
    let drop_optional_conn = open_migrated_conn();
    for tbl in [
        "world_meta",
        "owner_notes",
        "corp_lp_balances",
        "char_lp_logs",
        "wallet_journal_meta",
        "market_transactions",
        "account_plex_vault",
        "d6_corp_crimewatch_kv",
        "world_kv_blobs",
        "char_mining_ledger",
    ] {
        drop_optional_conn
            .execute(&format!("DROP TABLE {tbl}"), [])
            .unwrap_or_else(|e| panic!("drop optional {tbl}: {e}"));
    }
    let mut empty_dst = WorldStateSnapshot::default();
    load_d6(&drop_optional_conn, &mut empty_dst);
    load_ledger(&drop_optional_conn, &mut empty_dst);
    load_d6_corp_crimewatch(&drop_optional_conn, &mut empty_dst);
    load_d6_batch4(&drop_optional_conn, &mut empty_dst);

    // Direct social error check when a social table is dropped
    conn.execute("DROP TABLE mail_labels", [])
        .expect("drop mail_labels");
    assert!(load_d6_social_into_snapshot(&conn, &mut dst).is_err());
}

#[test]
fn sqlite_store_schema_and_jwt_secret_error_and_edge_branches() {
    let conn = open_migrated_conn();

    // Invalid hex lengths and invalid hex characters in auth_secrets trigger regeneration
    conn.execute(
        "INSERT OR REPLACE INTO auth_secrets (secret_key, secret_hex) VALUES ('jwt_signing_secret', 'short')",
        [],
    )
    .expect("insert short hex");
    let regen_short = load_or_init_jwt_secret(&conn, || [0xAB; 32]).expect("regen on short hex");
    assert_eq!(regen_short, [0xAB; 32]);

    conn.execute(
        "INSERT OR REPLACE INTO auth_secrets (secret_key, secret_hex) VALUES ('jwt_signing_secret', ?1)",
        params!["zz".repeat(32)],
    )
    .expect("insert non-hex 64-byte string");
    let regen_invalid =
        load_or_init_jwt_secret(&conn, || [0x5A; 32]).expect("regen on invalid hex chars");
    assert_eq!(regen_invalid, [0x5A; 32]);

    let loaded_existing =
        load_or_init_jwt_secret(&conn, || [0x00; 32]).expect("read persisted secret");
    assert_eq!(loaded_existing, [0x5A; 32]);

    save_jwt_secret(&conn, &[0x11; 32]).expect("save explicit secret");
    assert_eq!(
        load_or_init_jwt_secret(&conn, || [0x00; 32]).expect("reload explicit secret"),
        [0x11; 32]
    );

    // Shadow auth_secrets with a VIEW so writes to auth_secrets return Err
    conn.execute("DROP TABLE auth_secrets", [])
        .expect("drop auth_secrets");
    conn.execute(
        "CREATE VIEW auth_secrets AS SELECT 'jwt_signing_secret' AS secret_key, 'bad' AS secret_hex",
        [],
    )
    .expect("create view auth_secrets");
    assert!(save_jwt_secret(&conn, &[0x22; 32]).is_err());
    assert!(load_or_init_jwt_secret(&conn, || [0x22; 32]).is_err());

    // Sync error branches on ledger, d6, and social stores when their tables are missing
    let mut snap = build_populated_snapshot();
    snap.read_mails.insert((90_000_001, 1));
    let mut fp = crate::sqlite_store::fingerprints::TableFingerprints::default();
    let tx_err_conn = open_migrated_conn();
    tx_err_conn
        .execute("DROP TABLE account_plex_vault", [])
        .expect("drop account_plex_vault");
    let mut mut_conn = tx_err_conn;
    {
        let tx = mut_conn.transaction().expect("begin tx");
        assert!(sync_ledger(&tx, &snap, &mut fp).is_err());
    }
    mut_conn
        .execute("DROP TABLE owner_notes", [])
        .expect("drop owner_notes");
    {
        let tx = mut_conn.transaction().expect("begin tx");
        assert!(sync_d6(&tx, &snap, &mut fp).is_err());
    }
    mut_conn
        .execute("DROP TABLE read_mails", [])
        .expect("drop read_mails");
    {
        let tx = mut_conn.transaction().expect("begin tx");
        assert!(sync_d6_social(&tx, &snap, &mut fp).is_err());
    }
}

#[test]
fn sqlite_store_load_row_decode_error_branches() {
    // Inserting a BLOB into a typed TEXT/INTEGER column causes rusqlite `r.get::<_, T>(col)?`
    // inside `query_map` to yield `Err(InvalidColumnType)` during iteration, exercising every
    // `for r in rows { ... r.map_err(|e| e.to_string())? }` error path in `load.rs` & `social_store.rs`.
    let blob_row_inserts = [
        "INSERT OR REPLACE INTO world_meta (meta_key, meta_value) VALUES ('wal_seq', X'DEAD')",
        "INSERT INTO accounts (username, user_id, password_token, role, banned_until_unix, ban_reason) VALUES ('u', X'DEAD', 'p', 0, NULL, NULL)",
        "INSERT INTO characters (character_id, user_id, name, corporation_id, solar_system_id, active_ship_id, balance_cents, version, record_json) VALUES (1, 1, 'x', 1, 30000142, 1, 0, 1, X'DEAD')",
        "INSERT INTO skills (character_id, type_id, level, skill_points) VALUES (1, X'DEAD', 1, 0.0)",
        "INSERT INTO skill_queues (character_id, queue_index, type_id, target_level, start_sp, end_sp, duration_seconds, started_at_unix) VALUES (1, 0, X'DEAD', 1, 0.0, 100.0, 10.0, NULL)",
        "INSERT INTO jump_clones (character_id, clones_json) VALUES (1, X'DEAD')",
        "INSERT INTO active_implants (character_id, implants_json) VALUES (1, X'DEAD')",
        "INSERT INTO items (item_id, type_id, owner_id, location_id, flag, quantity, singleton, version, record_json) VALUES (1, 1, 1, 1, 1, 1, 0, 1, X'DEAD')",
        "INSERT INTO saved_fittings (fitting_id, owner_id, name, ship_type_id, description, record_json) VALUES (1, 1, 'f', 1, '', X'DEAD')",
        "INSERT INTO market_orders (order_id, owner_id, station_id, region_id, type_id, is_bid, price_cents, vol_entered, vol_remaining, min_volume, is_npc, escrow_cents) VALUES (1, X'DEAD', 1, 1, 34, 0, 10, 1, 1, 1, 0, 0)",
        "INSERT INTO wallet_journal (entry_id, seq_idx, character_id, entry_type, amount_cents, balance_after_cents, reason) VALUES (1, 0, X'DEAD', 't', 1, 1, 'r')",
        "INSERT INTO contracts (contract_id, issuer_id, acceptor_id, contract_type, status, start_station_id, end_station_id, price_cents, reward_cents, collateral_cents, expiration_unix, record_json) VALUES (1, 1, NULL, 'Courier', 'Outstanding', 60003760, 60003760, 0, 0, 0, 1, X'DEAD')",
        "INSERT INTO industry_jobs (job_id, installer_id, facility_id, blueprint_item_id, blueprint_type_id, activity_id, runs, product_type_id, status, duration_seconds, start_unix, blueprint_location_id, output_location_id) VALUES (1, X'DEAD', 1, 1, 1, 1, 1, 1, 'Active', 10, 10, 1, 1)",
        "INSERT INTO planetary_colonies (owner_id, planet_id, solar_system_id, command_center_level, pins_count, last_extracted_unix, record_json) VALUES (1, 1, 30000142, 1, 0, 1, X'DEAD')",
        "INSERT INTO missions (character_id, mission_id, agent_id, solar_system_id, state, current_room_id, npcs_remaining, record_json) VALUES (1, 1, 1, 30000142, 'Offered', 0, 0, X'DEAD')",
        "INSERT INTO mission_tally (character_id, tally) VALUES (1, X'DEAD')",
        "INSERT INTO standings (character_id, tx_json) VALUES (1, X'DEAD')",
        "INSERT INTO npe_progress (character_id, node_graph_id, current_node_id, completed, record_json) VALUES (1, 1, 1, 0, X'DEAD')",
        "INSERT INTO bookmarks (bookmark_id, owner_id, label, solar_system_id, x, y, z, item_id, memo, folder_id, is_shared) VALUES (1, X'DEAD', 'b', 30000142, 0.0, 0.0, 0.0, NULL, '', NULL, 0)",
        "INSERT INTO mail (message_id, seq_idx, sender_id, recipient_id, subject, body) VALUES (1, 0, X'DEAD', 1, 's', 'b')",
        "INSERT INTO fleets (fleet_id, boss_id, free_move, is_registered, record_json) VALUES (1, 1, 0, 0, X'DEAD')",
        "INSERT INTO corps (corporation_id, name, ticker, ceo_id, alliance_id, tax_rate, master_wallet_cents, record_json) VALUES (1, 'c', 'C', 1, NULL, 0.0, 0, X'DEAD')",
        "INSERT INTO alliances (alliance_id, name, short_name, executor_corp_id, record_json) VALUES (1, 'a', 'A', 1, X'DEAD')",
        "INSERT INTO wars (war_id, aggressor_corp_id, defender_corp_id, mutual, started_unix, active) VALUES (1, X'DEAD', 2, 0, 1, 1)",
        "INSERT INTO corp_projects (project_id, corporation_id, name, target_quantity, current_quantity, isk_per_unit_cents, escrow_remaining_cents) VALUES (1, X'DEAD', 'p', 1, 0, 1, 1)",
        "INSERT INTO killmails (kill_id, victim_character_id, attacker_character_id, solar_system_id, ship_type_id, total_value_isk, killed_unix, victim_corp_id, victim_alliance_id, attacker_corp_id, attacker_alliance_id) VALUES (1, X'DEAD', 1, 30000142, 587, 1.0, 1, 1, NULL, 1, NULL)",
        "INSERT INTO kill_rights (kill_right_id, owner_id, target_id, activation_fee_cents, is_active_suspect) VALUES (1, X'DEAD', 2, 1, 0)",
        "INSERT INTO structures (structure_id, type_id, owner_corp_id, solar_system_id, name, state, quantum_core_installed, fuel_blocks, shield_hp, armor_hp, hull_hp, record_json) VALUES (1, 35832, 1, 30000142, 's', 'ShieldVulnerable', 0, 0, 1.0, 1.0, 1.0, X'DEAD')",
        "INSERT INTO system_states (solar_system_id, dbs_multiplier, ess_main_bank_cents, ess_reserve_bank_cents, sov_alliance_id, record_json) VALUES (30000142, 1.0, 0, 0, NULL, X'DEAD')",
        "INSERT INTO idempotency_keys (character_id, idem_key) VALUES (1, X'DEAD')",
        "INSERT INTO licensed_skins (character_id, skins_json) VALUES (1, X'DEAD')",
        "INSERT INTO applied_ship_skins (ship_id, skin_id) VALUES (1, X'DEAD')",
        "INSERT INTO read_mails (character_id, mail_id) VALUES (1, X'DEAD')",
        "INSERT INTO deleted_mails (character_id, mail_id) VALUES (1, X'DEAD')",
        "INSERT INTO trashed_mails (character_id, mail_id) VALUES (1, X'DEAD')",
        "INSERT INTO mail_labels (character_id, labels_json) VALUES (1, X'DEAD')",
        "INSERT INTO mail_assigned_labels (character_id, mail_id, labels_json) VALUES (1, 1, X'DEAD')",
        "INSERT INTO mailing_lists (list_id, record_json) VALUES (1, X'DEAD')",
        "INSERT INTO calendar_events (event_id, record_json) VALUES (1, X'DEAD')",
        "INSERT INTO bookmark_meta (bookmark_id, record_json) VALUES (1, X'DEAD')",
        "INSERT INTO bookmark_folders (folder_id, record_json) VALUES (1, X'DEAD')",
        "INSERT INTO access_groups (group_id, members_json) VALUES (1, X'DEAD')",
        "INSERT INTO chat_channels (channel_id, record_json) VALUES ('c1', X'DEAD')",
        "INSERT INTO char_contacts (character_id, contacts_json) VALUES (1, X'DEAD')",
        "INSERT INTO char_contact_notifications (character_id, notifications_json) VALUES (1, X'DEAD')",
        "INSERT INTO char_contact_labels (character_id, labels_json) VALUES (1, X'DEAD')",
        "INSERT INTO corp_contacts (corporation_id, contacts_json) VALUES (1, X'DEAD')",
        "INSERT INTO corp_contact_labels (corporation_id, labels_json) VALUES (1, X'DEAD')",
        "INSERT INTO alliance_contacts (alliance_id, contacts_json) VALUES (1, X'DEAD')",
        "INSERT INTO alliance_contact_labels (alliance_id, labels_json) VALUES (1, X'DEAD')",
        "INSERT INTO char_notes (character_id, item_id, note) VALUES (1, 1, X'DEAD')",
    ];
    for sql in blob_row_inserts {
        let conn = open_migrated_conn();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute(sql, [])
            .unwrap_or_else(|e| panic!("insert blob row ({sql}): {e}"));
        assert!(
            load_snapshot_from_sqlite(&conn).is_err(),
            "expected load error for blob column SQL: {sql}"
        );
    }
}



