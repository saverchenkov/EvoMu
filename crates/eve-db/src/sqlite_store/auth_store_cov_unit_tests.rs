//! High-signal unit tests for `sqlite_store::auth_store` covering pruning,
//! expiration cleanup on load/replace, SHA-256 digest normalization, kicked users,
//! refresh tokens, and SQLite error branches.

use super::auth_store::*;
use crate::types::{AuthRuntimeState, SsoRefreshTokenRecord};
use rusqlite::Connection;

fn open_auth_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory sqlite");
    apply_auth_migration(&conn).expect("apply auth migration");
    conn
}

fn sample_refresh_record(key: &str, user_id: u64, expires_at_unix: u64) -> SsoRefreshTokenRecord {
    SsoRefreshTokenRecord {
        refresh_token: key.to_string(),
        user_id,
        character_id: 90_000_001,
        client_id: "evomu-client".to_string(),
        scopes: vec!["publicData".to_string(), "esi-wallet.read_character_wallet.v1".to_string()],
        expires_at_unix,
        revoked: false,
        last_access_token_jti: "jti-access-1".to_string(),
        last_access_token_exp_unix: expires_at_unix,
    }
}

#[test]
fn auth_store_lifecycle_pruning_and_roundtrip() {
    let mut conn = open_auth_conn();
    // Re-applying migration is idempotent even after column already exists
    apply_auth_migration(&conn).expect("idempotent auth migration");

    // 1. prune_auth_runtime_locked with now_unix == 0 returns 0
    let mut rt = AuthRuntimeState::new([0xAA; 32]);
    assert_eq!(prune_auth_runtime_locked(&mut rt, 0), 0);

    // 2. persist_prune_expired with now_unix == 0 returns Ok(0)
    assert_eq!(persist_prune_expired(&mut conn, 0).unwrap(), 0);

    // 3. Populate state with expired + active JTIs, kicked users, and raw + 64-hex refresh tokens
    let raw_key = "raw_secret_refresh_token_value";
    let hex_key = refresh_token_digest("already_hashed_token");
    rt.last_auth_now_unix = 1_000;
    rt.revoked_jtis.insert("jti-expired".to_string());
    rt.revoked_jti_expiries.insert("jti-expired".to_string(), 900);
    rt.revoked_jtis.insert("jti-active".to_string());
    rt.revoked_jti_expiries.insert("jti-active".to_string(), 2_000);
    rt.revoked_jtis.insert("jti-no-exp".to_string());
    rt.kicked_users.insert(42, (950, "gm kick".to_string()));
    rt.refresh_tokens
        .insert("expired-tok".to_string(), sample_refresh_record("expired-tok", 1, 500));
    rt.refresh_tokens
        .insert(raw_key.to_string(), sample_refresh_record(raw_key, 2, 2_500));
    rt.refresh_tokens
        .insert(hex_key.clone(), sample_refresh_record(&hex_key, 3, 0));

    // replace_auth_runtime_in_sqlite skips expired entries & hashes non-64-hex keys
    replace_auth_runtime_in_sqlite(&mut conn, &rt).unwrap();

    let loaded = load_auth_runtime_from_sqlite(&conn, [0xAA; 32]).unwrap();
    assert_eq!(loaded.last_auth_now_unix, 1_000);
    assert!(!loaded.revoked_jtis.contains("jti-expired"));
    assert!(loaded.revoked_jtis.contains("jti-active"));
    assert!(loaded.revoked_jtis.contains("jti-no-exp"));
    assert_eq!(loaded.kicked_users.get(&42), Some(&(950, "gm kick".to_string())));
    assert!(!loaded.refresh_tokens.contains_key("expired-tok"));
    assert!(loaded.refresh_tokens.contains_key(&refresh_token_digest(raw_key)));
    assert!(loaded.refresh_tokens.contains_key(&hex_key));

    // 4. Insert expired rows directly in SQLite and verify load_auth_runtime_from_sqlite prunes them
    conn.execute(
        "INSERT INTO auth_revoked_jtis (jti, expires_at_unix) VALUES ('jti-stale-on-load', 999)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO auth_refresh_tokens (token_hash, user_id, character_id, client_id, scopes_json, expires_at_unix, revoked, last_access_token_jti, last_access_token_exp_unix)
         VALUES ('stale-hash', 9, 90000009, 'c', '[]', 999, 0, 'j', 999)",
        [],
    )
    .unwrap();
    let loaded2 = load_auth_runtime_from_sqlite(&conn, [0xAA; 32]).unwrap();
    assert!(!loaded2.revoked_jtis.contains("jti-stale-on-load"));
    assert!(!loaded2.refresh_tokens.contains_key("stale-hash"));
    let jti_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM auth_revoked_jtis WHERE jti = 'jti-stale-on-load'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(jti_count, 0);

    // 5. Incremental persist helpers with last_auth_now_unix == 0 and > 0
    let minted_hash = refresh_token_digest("minted-1");
    let rec = sample_refresh_record(&minted_hash, 7, 3_000);
    persist_minted_refresh_token(&mut conn, &minted_hash, &rec, 0).unwrap();
    persist_minted_refresh_token(&mut conn, &refresh_token_digest("minted-2"), &rec, 1_500).unwrap();
    persist_refresh_token_revoked(&conn, &minted_hash).unwrap();
    persist_revoked_jti(&mut conn, "jti-new-1", 4_000, 0).unwrap();
    persist_revoked_jti(&mut conn, "jti-new-2", 4_000, 1_600).unwrap();
    persist_kicked_user(
        &mut conn,
        88,
        1_600,
        "ban",
        &[minted_hash.clone()],
        &[("jti-kick".to_string(), 1_900)],
        0,
    )
    .unwrap();
    persist_kicked_user(
        &mut conn,
        89,
        1_700,
        "ban2",
        &[],
        &[],
        1_700,
    )
    .unwrap();
    persist_clear_kicked_user(&conn, 88).unwrap();

    // 6. persist_prune_expired with now_unix > 0
    let pruned_db = persist_prune_expired(&mut conn, 2_600).unwrap();
    assert!(pruned_db >= 2);

    // 7. prune_auth_runtime_locked with now_unix > 0
    let mut rt_mem = loaded2;
    rt_mem.revoked_jtis.insert("jti-mem-exp".to_string());
    rt_mem.revoked_jti_expiries.insert("jti-mem-exp".to_string(), 1_500);
    rt_mem.refresh_tokens.insert(
        "mem-exp".to_string(),
        sample_refresh_record("mem-exp", 11, 1_500),
    );
    let pruned_mem = prune_auth_runtime_locked(&mut rt_mem, 2_100);
    assert!(pruned_mem >= 2);
}

#[test]
fn auth_store_error_branches_on_missing_tables_and_corrupt_json() {
    // Corrupt scopes_json -> Err
    {
        let conn = open_auth_conn();
        conn.execute(
            "INSERT INTO auth_refresh_tokens (token_hash, user_id, character_id, client_id, scopes_json, expires_at_unix, revoked, last_access_token_jti, last_access_token_exp_unix)
             VALUES ('h1', 1, 2, 'c', '{bad-json', 0, 0, 'j', 0)",
            [],
        )
        .unwrap();
        assert!(load_auth_runtime_from_sqlite(&conn, [0; 32]).is_err());
    }

    // Wrong column types trigger r.get errors across all 3 tables
    for sql in [
        "INSERT INTO auth_revoked_jtis (jti, expires_at_unix) VALUES ('j1', 'bad_int')",
        "INSERT INTO auth_kicked_users (user_id, kicked_at_unix, reason) VALUES (1, 'bad_int', 'r')",
        "INSERT INTO auth_refresh_tokens (token_hash, user_id, character_id, client_id, scopes_json, expires_at_unix, revoked, last_access_token_jti, last_access_token_exp_unix)
         VALUES ('h2', 'bad_int', 2, 'c', '[]', 0, 0, 'j', 0)",
    ] {
        let conn = open_auth_conn();
        conn.execute(sql, []).unwrap();
        assert!(load_auth_runtime_from_sqlite(&conn, [0; 32]).is_err());
    }

    // Missing tables trigger Err in load, replace, and incremental persist helpers
    for tbl in ["auth_revoked_jtis", "auth_kicked_users", "auth_refresh_tokens"] {
        let mut conn = open_auth_conn();
        conn.execute(&format!("DROP TABLE {tbl}"), []).unwrap();
        assert!(load_auth_runtime_from_sqlite(&conn, [0; 32]).is_err());
        let rt = AuthRuntimeState::new([0; 32]);
        assert!(replace_auth_runtime_in_sqlite(&mut conn, &rt).is_err());
    }

    let mut conn = Connection::open_in_memory().unwrap();
    let rec = sample_refresh_record("h", 1, 100);
    assert!(persist_minted_refresh_token(&mut conn, "h", &rec, 10).is_err());
    assert!(persist_refresh_token_revoked(&conn, "h").is_err());
    assert!(persist_revoked_jti(&mut conn, "j", 10, 10).is_err());
    assert!(persist_kicked_user(&mut conn, 1, 10, "r", &[], &[], 10).is_err());
    assert!(persist_clear_kicked_user(&conn, 1).is_err());
    assert!(persist_prune_expired(&mut conn, 10).is_err());
    assert!(apply_auth_migration(&conn).is_ok());
    // Dropping auth_runtime_meta fails prune_in_tx inside persist_prune_expired
    conn.execute("DROP TABLE auth_runtime_meta", []).unwrap();
    assert!(persist_prune_expired(&mut conn, 10).is_err());
}
