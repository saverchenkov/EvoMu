use super::*;
use rusqlite::{Connection, OpenFlags};

#[test]
fn apply_migrations_upgrades_legacy_tables_without_extra_columns() {
    let conn = Connection::open_in_memory().unwrap();
    // Create pre-migration minimal versions of the 5 legacy tables that apply_migrations alters.
    conn.execute_batch(
        "
        CREATE TABLE skill_queues (
            character_id INTEGER PRIMARY KEY
        );
        CREATE TABLE characters (
            character_id INTEGER PRIMARY KEY,
            account_id INTEGER NOT NULL
        );
        CREATE TABLE killmails (
            killmail_id INTEGER PRIMARY KEY
        );
        CREATE TABLE bookmarks (
            bookmark_id INTEGER PRIMARY KEY
        );
        CREATE TABLE industry_jobs (
            job_id INTEGER PRIMARY KEY
        );
        ",
    )
    .unwrap();

    apply_migrations(&conn).unwrap();

    // Verify all legacy ALTER TABLE columns were added and can be selected.
    conn.execute_batch(
        "
        SELECT started_at_unix FROM skill_queues;
        SELECT activity_status FROM characters;
        SELECT killed_unix, victim_corp_id, victim_alliance_id, attacker_corp_id, attacker_alliance_id FROM killmails;
        SELECT item_id, memo, folder_id, is_shared FROM bookmarks;
        SELECT blueprint_location_id, output_location_id, cost, licensed_runs, probability, completed_unix, completed_character_id FROM industry_jobs;
        ",
    )
    .unwrap();

    // Second call is idempotent when version == CURRENT_SCHEMA_VERSION.
    apply_migrations(&conn).unwrap();
}

#[test]
fn apply_migrations_fails_on_readonly_and_shadowed_tables() {
    let tmp = std::env::temp_dir().join(format!(
        "evomu_schema_ro_{}_{:?}.sqlite",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&tmp);
    {
        let rw = Connection::open(&tmp).unwrap();
        rw.execute_batch("CREATE TABLE dummy (id INTEGER);").unwrap();
    }
    let ro = Connection::open_with_flags(&tmp, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert!(apply_migrations(&ro).is_err());
    drop(ro);
    let _ = std::fs::remove_file(&tmp);

    // VIEW shadowing schema_migrations causes DDL failure.
    let conn1 = Connection::open_in_memory().unwrap();
    conn1
        .execute_batch("CREATE VIEW schema_migrations AS SELECT 0 AS version, 0 AS applied_at;")
        .unwrap();
    assert!(apply_migrations(&conn1).is_err());

    // Wrong-schema schema_migrations fails version query.
    let conn2 = Connection::open_in_memory().unwrap();
    conn2
        .execute_batch("CREATE TABLE schema_migrations (bad INTEGER);")
        .unwrap();
    assert!(apply_migrations(&conn2).is_err());

    // schema_migrations missing applied_unix fails at INSERT.
    let conn3 = Connection::open_in_memory().unwrap();
    conn3
        .execute_batch("CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, name TEXT);")
        .unwrap();
    assert!(apply_migrations(&conn3).is_err());
}

#[test]
fn load_or_init_jwt_secret_and_save_edges() {
    // Missing auth_secrets table returns Err
    let no_table = Connection::open_in_memory().unwrap();
    assert!(load_or_init_jwt_secret(&no_table, || [0x11; 32]).is_err());
    assert!(save_jwt_secret(&no_table, &[0x11; 32]).is_err());

    // Valid init + reload + leading-zero formatting roundtrip
    let conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();
    let expected = [
        0x00, 0x01, 0x0f, 0x10, 0xab, 0xcd, 0xef, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09,
        0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa,
        0xbb, 0x00,
    ];
    let loaded = load_or_init_jwt_secret(&conn, || expected).unwrap();
    assert_eq!(loaded, expected);
    // Second call ignores generator and returns stored value
    let reloaded = load_or_init_jwt_secret(&conn, || [0xff; 32]).unwrap();
    assert_eq!(reloaded, expected);

    // 63-byte hex fails & regenerates
    conn.execute(
        "INSERT OR REPLACE INTO auth_secrets (secret_key, secret_hex) VALUES ('jwt_signing_secret', ?1)",
        rusqlite::params!["a".repeat(63)],
    )
    .unwrap();
    let regen_short = load_or_init_jwt_secret(&conn, || [0x22; 32]).unwrap();
    assert_eq!(regen_short, [0x22; 32]);

    // 65-byte hex fails & regenerates
    conn.execute(
        "INSERT OR REPLACE INTO auth_secrets (secret_key, secret_hex) VALUES ('jwt_signing_secret', ?1)",
        rusqlite::params!["a".repeat(65)],
    )
    .unwrap();
    let regen_long = load_or_init_jwt_secret(&conn, || [0x33; 32]).unwrap();
    assert_eq!(regen_long, [0x33; 32]);

    // 64-byte string with multibyte UTF-8 ('é' = 2 bytes -> 62 + 2 = 64 bytes, non-char-boundary pairs / non-hex)
    let bad_utf8_len64 = format!("{}é", "a".repeat(62));
    assert_eq!(bad_utf8_len64.len(), 64);
    conn.execute(
        "INSERT OR REPLACE INTO auth_secrets (secret_key, secret_hex) VALUES ('jwt_signing_secret', ?1)",
        rusqlite::params![bad_utf8_len64],
    )
    .unwrap();
    let regen_utf8 = load_or_init_jwt_secret(&conn, || [0x44; 32]).unwrap();
    assert_eq!(regen_utf8, [0x44; 32]);

    // Non-hex ASCII len 64 also fails & regenerates
    conn.execute(
        "INSERT OR REPLACE INTO auth_secrets (secret_key, secret_hex) VALUES ('jwt_signing_secret', ?1)",
        rusqlite::params!["z".repeat(64)],
    )
    .unwrap();
    let regen_nonhex = load_or_init_jwt_secret(&conn, || [0x55; 32]).unwrap();
    assert_eq!(regen_nonhex, [0x55; 32]);
}
