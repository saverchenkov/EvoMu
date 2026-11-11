//! Unit tests for `sqlite_store::auth_store` pruning, load cleanup, and persistence paths.

use super::*;
use rusqlite::{params, Connection};

fn open_auth_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory sqlite");
    apply_auth_migration(&conn).expect("apply auth migration");
    conn
}

#[test]
fn auth_store_prune_expired_load_cleanup_replace_and_incremental_persist() {
    let mut conn = open_auth_conn();
    let secret = [0x42u8; 32];

    // 1. Zero timestamp is a safe no-op for both in-memory and SQLite pruning.
    let mut rt = AuthRuntimeState::new(secret);
    assert_eq!(prune_auth_runtime_locked(&mut rt, 0), 0);
    assert_eq!(persist_prune_expired(&mut conn, 0).unwrap(), 0);

    // 2. Seed SQLite with last_auth_now_unix = 2000 plus expired & live JTIs and refresh tokens.
    conn.execute(
        "INSERT INTO auth_runtime_meta (meta_key, meta_value) VALUES ('last_auth_now_unix', 2000)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO auth_revoked_jtis (jti, expires_at_unix) VALUES ('jti_expired', 1500), ('jti_live', 3000)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO auth_kicked_users (user_id, kicked_at_unix, reason) VALUES (77, 1900, 'test_kick')",
        [],
    )
    .unwrap();
    let hash_expired = refresh_token_digest("raw_expired_tok");
    let hash_live = refresh_token_digest("raw_live_tok");
    conn.execute(
        "INSERT INTO auth_refresh_tokens (token_hash, user_id, character_id, client_id, scopes_json, expires_at_unix, revoked, last_access_token_jti, last_access_token_exp_unix)
         VALUES (?1, 1, 90000001, 'eve-client', '[\"publicData\"]', 1500, 0, 'jti_old', 1500),
                (?2, 1, 90000001, 'eve-client', '[\"publicData\"]', 3500, 0, 'jti_cur', 3500)",
        params![hash_expired, hash_live],
    )
    .unwrap();

    let loaded = load_auth_runtime_from_sqlite(&conn, secret).expect("load auth runtime");
    assert_eq!(loaded.last_auth_now_unix, 2000);
    assert!(!loaded.revoked_jtis.contains("jti_expired"));
    assert!(loaded.revoked_jtis.contains("jti_live"));
    assert!(!loaded.refresh_tokens.contains_key(&hash_expired));
    assert!(loaded.refresh_tokens.contains_key(&hash_live));
    assert_eq!(loaded.kicked_users.get(&77).map(|x| x.0), Some(1900));

    // Verify expired entries were deleted from SQLite on load.
    let jti_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM auth_revoked_jtis", [], |r| r.get(0))
        .unwrap();
    let rt_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM auth_refresh_tokens", [], |r| r.get(0))
        .unwrap();
    assert_eq!(jti_count, 1);
    assert_eq!(rt_count, 1);

    // 3. replace_auth_runtime_in_sqlite skips expired entries and hashes non-64-hex keys.
    let mut rt_replace = AuthRuntimeState::new(secret);
    rt_replace.last_auth_now_unix = 2000;
    rt_replace.revoked_jtis.insert("jti_exp_rep".into());
    rt_replace
        .revoked_jti_expiries
        .insert("jti_exp_rep".into(), 1800);
    rt_replace.revoked_jtis.insert("jti_live_rep".into());
    rt_replace
        .revoked_jti_expiries
        .insert("jti_live_rep".into(), 5000);
    rt_replace.kicked_users.insert(88, (1950, "kicked".into()));
    rt_replace.refresh_tokens.insert(
        "expired_key".into(),
        SsoRefreshTokenRecord {
            refresh_token: "expired_key".into(),
            user_id: 2,
            character_id: 90_000_002,
            client_id: "c1".into(),
            scopes: vec!["s1".into()],
            expires_at_unix: 1000,
            revoked: false,
            last_access_token_jti: "j1".into(),
            last_access_token_exp_unix: 1000,
        },
    );
    rt_replace.refresh_tokens.insert(
        "legacy_plaintext_key".into(),
        SsoRefreshTokenRecord {
            refresh_token: "legacy_plaintext_key".into(),
            user_id: 2,
            character_id: 90_000_002,
            client_id: "c1".into(),
            scopes: vec!["s1".into()],
            expires_at_unix: 6000,
            revoked: false,
            last_access_token_jti: "j2".into(),
            last_access_token_exp_unix: 6000,
        },
    );
    replace_auth_runtime_in_sqlite(&mut conn, &rt_replace).expect("replace auth runtime");

    let reloaded = load_auth_runtime_from_sqlite(&conn, secret).expect("reload replaced");
    assert!(!reloaded.revoked_jtis.contains("jti_exp_rep"));
    assert!(reloaded.revoked_jtis.contains("jti_live_rep"));
    let expected_legacy_hash = refresh_token_digest("legacy_plaintext_key");
    assert!(reloaded.refresh_tokens.contains_key(&expected_legacy_hash));
    assert_eq!(reloaded.refresh_tokens.len(), 1);

    // 4. Incremental persist_* calls with last_auth_now_unix > 0 trigger prune_in_tx.
    let minted_hash = refresh_token_digest("minted_raw");
    let minted_rec = SsoRefreshTokenRecord {
        refresh_token: minted_hash.clone(),
        user_id: 3,
        character_id: 90_000_003,
        client_id: "c2".into(),
        scopes: vec!["esi-wallet.read_character_wallet.v1".into()],
        expires_at_unix: 7000,
        revoked: false,
        last_access_token_jti: "j3".into(),
        last_access_token_exp_unix: 7000,
    };
    persist_minted_refresh_token(&mut conn, &minted_hash, &minted_rec, 2100)
        .expect("persist minted with prune");
    persist_refresh_token_revoked(&conn, &minted_hash).expect("revoke refresh token");
    persist_revoked_jti(&mut conn, "jti_short", 5500, 2200).expect("persist revoked jti");
    persist_kicked_user(
        &mut conn,
        99,
        2300,
        "kick_reason",
        &[minted_hash.clone()],
        &[("jti_from_kick".into(), 5800)],
        2300,
    )
    .expect("persist kicked user");
    persist_clear_kicked_user(&conn, 99).expect("clear kicked user");

    // 5. persist_prune_expired at 6500 prunes jti_live_rep (5000), jti_short (5500), jti_from_kick (5800),
    //    and legacy_plaintext_key (6000) while preserving minted_hash (7000).
    let pruned = persist_prune_expired(&mut conn, 6500).expect("persist prune expired");
    assert_eq!(pruned, 4);

    let after_prune = load_auth_runtime_from_sqlite(&conn, secret).expect("load after prune");
    assert_eq!(after_prune.last_auth_now_unix, 6500);
    assert!(after_prune.revoked_jtis.is_empty());
    assert_eq!(after_prune.refresh_tokens.len(), 1);
    assert!(after_prune.refresh_tokens[&minted_hash].revoked);
    assert!(!after_prune.kicked_users.contains_key(&99));
}

#[test]
fn auth_store_sql_and_corrupt_row_error_paths() {
    let secret = [0x11u8; 32];

    // 1. Corrupt scopes_json and BLOB column types during load_auth_runtime_from_sqlite
    {
        let conn = open_auth_conn();
        conn.execute(
            "INSERT INTO auth_refresh_tokens (token_hash, user_id, character_id, client_id, scopes_json, expires_at_unix, revoked, last_access_token_jti, last_access_token_exp_unix)
             VALUES ('h1', 1, 1, 'c', '{not-json', 9999, 0, 'j', 0)",
            [],
        )
        .unwrap();
        assert!(load_auth_runtime_from_sqlite(&conn, secret).is_err());
    }
    for sql in [
        "INSERT INTO auth_revoked_jtis (jti, expires_at_unix) VALUES (X'DEAD', 100)",
        "INSERT INTO auth_kicked_users (user_id, kicked_at_unix, reason) VALUES (1, 100, X'DEAD')",
        "INSERT INTO auth_refresh_tokens (token_hash, user_id, character_id, client_id, scopes_json, expires_at_unix, revoked, last_access_token_jti, last_access_token_exp_unix) VALUES (X'DEAD', 1, 1, 'c', '[]', 100, 0, 'j', 0)",
    ] {
        let conn = open_auth_conn();
        conn.execute(sql, []).unwrap();
        assert!(load_auth_runtime_from_sqlite(&conn, secret).is_err());
    }

    // 2. Dropped auth tables exercise prepare/execute map_err closures across all auth_store functions
    for tbl in ["auth_revoked_jtis", "auth_kicked_users", "auth_refresh_tokens"] {
        let conn = open_auth_conn();
        conn.execute(&format!("DROP TABLE {tbl}"), []).unwrap();
        assert!(load_auth_runtime_from_sqlite(&conn, secret).is_err());
    }

    let mut rt = AuthRuntimeState::new(secret);
    rt.last_auth_now_unix = 500;
    rt.revoked_jtis.insert("j1".into());
    rt.revoked_jti_expiries.insert("j1".into(), 900);
    rt.kicked_users.insert(1, (400, "r".into()));
    let rec = SsoRefreshTokenRecord {
        refresh_token: "tok".into(),
        user_id: 1,
        character_id: 10,
        client_id: "c".into(),
        scopes: vec![],
        expires_at_unix: 900,
        revoked: false,
        last_access_token_jti: "j".into(),
        last_access_token_exp_unix: 900,
    };
    rt.refresh_tokens.insert(refresh_token_digest("tok"), rec.clone());

    for tbl in [
        "auth_runtime_meta",
        "auth_revoked_jtis",
        "auth_kicked_users",
        "auth_refresh_tokens",
    ] {
        let mut conn = open_auth_conn();
        conn.execute(&format!("DROP TABLE {tbl}"), []).unwrap();
        assert!(replace_auth_runtime_in_sqlite(&mut conn, &rt).is_err());
    }

    let mut conn = open_auth_conn();
    conn.execute("DROP TABLE auth_refresh_tokens", []).unwrap();
    conn.execute("DROP TABLE auth_revoked_jtis", []).unwrap();
    conn.execute("DROP TABLE auth_kicked_users", []).unwrap();
    assert!(persist_minted_refresh_token(&mut conn, "h", &rec, 500).is_err());
    assert!(persist_refresh_token_revoked(&conn, "h").is_err());
    assert!(persist_revoked_jti(&mut conn, "j", 900, 500).is_err());
    assert!(persist_kicked_user(&mut conn, 1, 500, "r", &["h".into()], &[("j".into(), 900)], 500).is_err());
    assert!(persist_clear_kicked_user(&conn, 1).is_err());
    assert!(persist_prune_expired(&mut conn, 500).is_err());
}
