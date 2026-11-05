//! Unit and mutation-killing tests for `sqlite_store::schema`.

use super::*;
use rusqlite::Connection;

#[test]
fn apply_migrations_creates_schema_idempotently_and_seeds_world_meta() {
    let conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    let (ver, name, applied_unix): (i64, String, i64) = conn
        .query_row(
            "SELECT version, name, applied_unix FROM schema_migrations WHERE version = ?1",
            params![CURRENT_SCHEMA_VERSION],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(ver, 1);
    assert_eq!(name, "0001_initial_world_schema");
    assert!(applied_unix > 0);

    let next_id: i64 = conn
        .query_row(
            "SELECT meta_value FROM world_meta WHERE meta_key = 'next_entity_id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let wal_seq: i64 = conn
        .query_row(
            "SELECT meta_value FROM world_meta WHERE meta_key = 'wal_seq'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let markets_seeded: i64 = conn
        .query_row(
            "SELECT meta_value FROM world_meta WHERE meta_key = 'markets_seeded'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(next_id, 200_000_000);
    assert_eq!(wal_seq, 0);
    assert_eq!(markets_seeded, 0);

    // Update world_meta and verify re-running apply_migrations is idempotent (INSERT OR IGNORE)
    conn.execute(
        "UPDATE world_meta SET meta_value = 42 WHERE meta_key = 'wal_seq'",
        [],
    )
    .unwrap();
    apply_migrations(&conn).unwrap();
    let wal_seq_after: i64 = conn
        .query_row(
            "SELECT meta_value FROM world_meta WHERE meta_key = 'wal_seq'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(wal_seq_after, 42);
}

#[test]
fn jwt_secret_load_save_and_fallback_validation_boundaries() {
    let conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    // 1. Missing secret -> calls fallback_gen and persists 64-char hex
    let mut gen_called = false;
    let first_secret = [0xABu8; 32];
    let loaded = load_or_init_jwt_secret(&conn, || {
        gen_called = true;
        first_secret
    })
    .unwrap();
    assert!(gen_called);
    assert_eq!(loaded, first_secret);

    let stored_hex: String = conn
        .query_row(
            "SELECT secret_hex FROM auth_secrets WHERE secret_key = 'jwt_signing_secret'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored_hex.len(), 64);
    assert_eq!(stored_hex, "ab".repeat(32));

    // 2. Existing valid 64-char hex -> returns stored secret without calling fallback_gen
    let loaded_again = load_or_init_jwt_secret(&conn, || {
        panic!("fallback_gen must not be called when valid 64-char hex exists");
    })
    .unwrap();
    assert_eq!(loaded_again, first_secret);

    // 3. save_jwt_secret overwrites with distinct byte pattern 0..32
    let mut seq_secret = [0u8; 32];
    for (i, b) in seq_secret.iter_mut().enumerate() {
        *b = i as u8;
    }
    save_jwt_secret(&conn, &seq_secret).unwrap();
    let after_save = load_or_init_jwt_secret(&conn, || [0xFFu8; 32]).unwrap();
    assert_eq!(after_save, seq_secret);

    // 4. Invalid length (< 64 or > 64) triggers fallback regeneration
    conn.execute(
        "UPDATE auth_secrets SET secret_hex = 'deadbeef' WHERE secret_key = 'jwt_signing_secret'",
        [],
    )
    .unwrap();
    let short_fallback = [0x34u8; 32];
    let regenerated = load_or_init_jwt_secret(&conn, || short_fallback).unwrap();
    assert_eq!(regenerated, short_fallback);

    // 5. Length == 64 but non-hex characters triggers fallback regeneration
    let invalid_hex_64 = "zz".repeat(32);
    assert_eq!(invalid_hex_64.len(), 64);
    conn.execute(
        "UPDATE auth_secrets SET secret_hex = ?1 WHERE secret_key = 'jwt_signing_secret'",
        params![invalid_hex_64],
    )
    .unwrap();
    let non_hex_fallback = [0x56u8; 32];
    let regenerated2 = load_or_init_jwt_secret(&conn, || non_hex_fallback).unwrap();
    assert_eq!(regenerated2, non_hex_fallback);
}
