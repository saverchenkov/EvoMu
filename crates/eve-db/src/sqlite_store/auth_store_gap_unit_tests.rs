//! Comprehensive unit tests for `auth_store` (`apply_auth_migration`, `prune_auth_runtime_locked`,
//! `load_auth_runtime_from_sqlite`, `replace_auth_runtime_in_sqlite`, incremental persistence
//! helpers, and `prune_in_tx` monotonicity & missing-table error branches).

#![forbid(unsafe_code)]

use super::*;
use rusqlite::{params, Connection, OpenFlags};
use std::sync::atomic::{AtomicU64, Ordering};

fn open_auth_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory sqlite");
    apply_auth_migration(&conn).expect("apply auth migration");
    conn
}

fn sample_refresh_record(
    token: &str,
    user_id: u64,
    exp_unix: u64,
    revoked: bool,
) -> SsoRefreshTokenRecord {
    SsoRefreshTokenRecord {
        refresh_token: token.to_string(),
        user_id,
        character_id: 90_000_001,
        client_id: "eve-online-client".to_string(),
        scopes: vec!["esi-skills.read_skills.v1".to_string()],
        expires_at_unix: exp_unix,
        revoked,
        last_access_token_jti: format!("jti-{user_id}-{exp_unix}"),
        last_access_token_exp_unix: exp_unix.saturating_add(600),
    }
}

fn temp_sqlite_path(tag: &str) -> std::path::PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "evomu_auth_store_{tag}_{}_{}.sqlite",
        std::process::id(),
        n
    ))
}

#[test]
fn group_a_prune_auth_runtime_locked_is_monotonic_and_prunes_boundaries() {
    let digest = refresh_token_digest("raw-refresh-token-1");
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    assert_eq!(digest, refresh_token_digest("raw-refresh-token-1"));
    assert_ne!(digest, refresh_token_digest("raw-refresh-token-2"));

    let mut rt = AuthRuntimeState::new([7u8; 32]);
    assert_eq!(prune_auth_runtime_locked(&mut rt, 0), 0);
    assert_eq!(rt.last_auth_now_unix, 0);

    rt.revoked_jtis.extend([
        "jti-no-exp".to_string(),
        "jti-zero".to_string(),
        "jti-eq".to_string(),
        "jti-future".to_string(),
    ]);
    rt.revoked_jti_expiries.insert("jti-zero".to_string(), 0);
    rt.revoked_jti_expiries.insert("jti-eq".to_string(), 1_000);
    rt.revoked_jti_expiries.insert("jti-future".to_string(), 1_001);

    rt.refresh_tokens
        .insert("rt-zero".to_string(), sample_refresh_record("rt-zero", 1, 0, false));
    rt.refresh_tokens
        .insert("rt-past".to_string(), sample_refresh_record("rt-past", 1, 999, false));
    rt.refresh_tokens
        .insert("rt-eq".to_string(), sample_refresh_record("rt-eq", 1, 1_000, false));
    rt.refresh_tokens
        .insert("rt-future".to_string(), sample_refresh_record("rt-future", 1, 1_500, false));

    let pruned = prune_auth_runtime_locked(&mut rt, 1_000);
    assert_eq!(pruned, 2); // jti-eq (<= 1000) + rt-past (< 1000)
    assert_eq!(rt.last_auth_now_unix, 1_000);
    assert!(rt.revoked_jtis.contains("jti-no-exp"));
    assert!(rt.revoked_jtis.contains("jti-zero"));
    assert!(rt.revoked_jtis.contains("jti-future"));
    assert!(!rt.revoked_jtis.contains("jti-eq"));
    assert!(rt.refresh_tokens.contains_key("rt-zero"));
    assert!(rt.refresh_tokens.contains_key("rt-eq")); // cutoff <= expires_at_unix retains equality
    assert!(rt.refresh_tokens.contains_key("rt-future"));
    assert!(!rt.refresh_tokens.contains_key("rt-past"));

    // Smaller now_unix must NOT regress last_auth_now_unix and still uses cutoff=1000
    assert_eq!(prune_auth_runtime_locked(&mut rt, 250), 0);
    assert_eq!(rt.last_auth_now_unix, 1_000);
}

#[test]
fn group_b_apply_auth_migration_readonly_error_and_legacy_alter_idempotent() {
    // 1. Read-only connection returns Err via execute_batch().map_err
    let ro_path = temp_sqlite_path("ro");
    {
        let rw = Connection::open(&ro_path).expect("create sqlite file");
        rw.execute_batch("CREATE TABLE dummy (id INTEGER PRIMARY KEY);")
            .expect("seed dummy table");
    }
    let ro_conn =
        Connection::open_with_flags(&ro_path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open ro");
    let ro_err = apply_auth_migration(&ro_conn).expect_err("read-only migration must fail");
    assert!(!ro_err.is_empty());
    drop(ro_conn);
    let _ = std::fs::remove_file(&ro_path);

    // 2. Legacy auth_refresh_tokens table without last_access_token_exp_unix triggers ALTER TABLE
    let conn = Connection::open_in_memory().expect("open in-memory");
    conn.execute_batch(
        "CREATE TABLE auth_refresh_tokens (
            token_hash TEXT PRIMARY KEY,
            user_id INTEGER NOT NULL,
            character_id INTEGER NOT NULL,
            client_id TEXT NOT NULL,
            scopes_json TEXT NOT NULL,
            expires_at_unix INTEGER NOT NULL,
            revoked INTEGER NOT NULL,
            last_access_token_jti TEXT NOT NULL
        );",
    )
    .expect("create legacy table");

    apply_auth_migration(&conn).expect("upgrade legacy table");
    // Re-running migration hits the duplicate-column ignore branch cleanly
    apply_auth_migration(&conn).expect("idempotent second migration");

    let has_col: bool = conn
        .prepare("PRAGMA table_info(auth_refresh_tokens)")
        .expect("pragma")
        .query_map([], |r| r.get::<_, String>(1))
        .expect("map")
        .filter_map(Result::ok)
        .any(|c| c == "last_access_token_exp_unix");
    assert!(has_col);
}

#[test]
fn group_c_load_auth_runtime_clamps_negatives_enforces_boundaries_and_surfaces_errors() {
    // Missing tables fail cleanly
    let empty_conn = Connection::open_in_memory().expect("open empty");
    assert!(load_auth_runtime_from_sqlite(&empty_conn, [1u8; 32]).is_err());

    // Negative last_auth_now_unix in meta clamps to 0
    let conn = open_auth_conn();
    conn.execute(
        "INSERT INTO auth_runtime_meta (meta_key, meta_value) VALUES ('last_auth_now_unix', -50)",
        [],
    )
    .expect("insert negative meta");
    let rt_neg = load_auth_runtime_from_sqlite(&conn, [2u8; 32]).expect("load with negative meta");
    assert_eq!(rt_neg.last_auth_now_unix, 0);

    // Set last_auth_now_unix = 500 and test boundary pruning on load
    conn.execute(
        "INSERT OR REPLACE INTO auth_runtime_meta (meta_key, meta_value) VALUES ('last_auth_now_unix', 500)",
        [],
    )
    .expect("set last_now=500");
    conn.execute("INSERT INTO auth_revoked_jtis (jti, expires_at_unix) VALUES ('jti-neg', -5)", [])
        .unwrap();
    conn.execute("INSERT INTO auth_revoked_jtis (jti, expires_at_unix) VALUES ('jti-zero', 0)", [])
        .unwrap();
    conn.execute("INSERT INTO auth_revoked_jtis (jti, expires_at_unix) VALUES ('jti-eq', 500)", [])
        .unwrap();
    conn.execute("INSERT INTO auth_revoked_jtis (jti, expires_at_unix) VALUES ('jti-gt', 501)", [])
        .unwrap();

    conn.execute(
        "INSERT INTO auth_kicked_users (user_id, kicked_at_unix, reason) VALUES (77, 450, 'AdminKick')",
        [],
    )
    .unwrap();

    let scopes = serde_json::to_string(&vec!["esi-assets.read_assets.v1"]).unwrap();
    for (hash, exp) in [("h-zero", 0_i64), ("h-eq", 500_i64), ("h-lt", 499_i64)] {
        conn.execute(
            "INSERT INTO auth_refresh_tokens (token_hash, user_id, character_id, client_id, scopes_json, expires_at_unix, revoked, last_access_token_jti, last_access_token_exp_unix)
             VALUES (?1, 77, 90000001, 'client', ?2, ?3, 0, 'j', 900)",
            params![hash, scopes, exp],
        )
        .unwrap();
    }

    let rt = load_auth_runtime_from_sqlite(&conn, [3u8; 32]).expect("load boundary rows");
    assert_eq!(rt.last_auth_now_unix, 500);
    assert!(rt.revoked_jtis.contains("jti-neg"));
    assert!(rt.revoked_jtis.contains("jti-zero"));
    assert!(rt.revoked_jtis.contains("jti-gt"));
    assert!(!rt.revoked_jtis.contains("jti-eq"));
    assert!(!rt.revoked_jti_expiries.contains_key("jti-neg"));
    assert!(!rt.revoked_jti_expiries.contains_key("jti-zero"));
    assert_eq!(rt.revoked_jti_expiries.get("jti-gt"), Some(&501));
    assert_eq!(rt.kicked_users.get(&77), Some(&(450, "AdminKick".to_string())));

    // Refresh token boundary: > last_now only prunes strictly < 500, keeping exp == 500
    assert!(rt.refresh_tokens.contains_key("h-zero"));
    assert!(rt.refresh_tokens.contains_key("h-eq"));
    assert!(!rt.refresh_tokens.contains_key("h-lt"));

    // Verify expired JTI & refresh token were also deleted from SQLite during load
    let rem_jtis: i64 = conn
        .query_row("SELECT COUNT(*) FROM auth_revoked_jtis WHERE jti = 'jti-eq'", [], |r| r.get(0))
        .unwrap();
    let rem_rts: i64 = conn
        .query_row("SELECT COUNT(*) FROM auth_refresh_tokens WHERE token_hash = 'h-lt'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rem_jtis, 0);
    assert_eq!(rem_rts, 0);

    // Corrupt scopes JSON returns Err
    conn.execute(
        "INSERT INTO auth_refresh_tokens (token_hash, user_id, character_id, client_id, scopes_json, expires_at_unix, revoked, last_access_token_jti, last_access_token_exp_unix)
         VALUES ('h-bad-json', 78, 90000001, 'client', '{not-json', 0, 0, 'j', 0)",
        [],
    )
    .unwrap();
    assert!(load_auth_runtime_from_sqlite(&conn, [3u8; 32]).is_err());
}

#[test]
fn group_d_replace_auth_runtime_handles_zero_now_missing_expiries_and_digest_fastpath() {
    let mut conn = open_auth_conn();

    // 1. last_auth_now_unix == 0 omits auth_runtime_meta row and handles missing expiry map entries
    let mut rt_zero = AuthRuntimeState::new([4u8; 32]);
    rt_zero.revoked_jtis.insert("jti-untracked-exp".to_string());
    replace_auth_runtime_in_sqlite(&mut conn, &rt_zero).expect("replace with zero now");
    let meta_cnt: i64 = conn
        .query_row("SELECT COUNT(*) FROM auth_runtime_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(meta_cnt, 0);

    // 2. Non-zero now + 64-hex fastpath vs legacy non-hex digest + expired entries filtered on save
    let mut rt = AuthRuntimeState::new([5u8; 32]);
    rt.last_auth_now_unix = 1_000;
    rt.revoked_jtis.extend([
        "jti-expired".to_string(),
        "jti-kept".to_string(),
        "jti-no-exp".to_string(),
    ]);
    rt.revoked_jti_expiries.insert("jti-expired".to_string(), 1_000);
    rt.revoked_jti_expiries.insert("jti-kept".to_string(), 2_000);
    rt.kicked_users.insert(99, (950, "Displaced".to_string()));

    let hex_key = refresh_token_digest("already-hashed");
    rt.refresh_tokens
        .insert(hex_key.clone(), sample_refresh_record(&hex_key, 99, 1_000, false));
    rt.refresh_tokens
        .insert("legacy-raw-token".to_string(), sample_refresh_record("legacy-raw-token", 99, 0, true));
    rt.refresh_tokens
        .insert("expired-raw".to_string(), sample_refresh_record("expired-raw", 99, 999, false));

    replace_auth_runtime_in_sqlite(&mut conn, &rt).expect("replace runtime");
    let reloaded = load_auth_runtime_from_sqlite(&conn, [5u8; 32]).expect("reload");

    assert_eq!(reloaded.last_auth_now_unix, 1_000);
    assert!(!reloaded.revoked_jtis.contains("jti-expired"));
    assert!(reloaded.revoked_jtis.contains("jti-kept"));
    assert!(reloaded.revoked_jtis.contains("jti-no-exp"));
    assert!(reloaded.refresh_tokens.contains_key(&hex_key));
    let expected_legacy_digest = refresh_token_digest("legacy-raw-token");
    assert!(reloaded.refresh_tokens.contains_key(&expected_legacy_digest));
    assert!(reloaded.refresh_tokens[&expected_legacy_digest].revoked);
    assert!(!reloaded.refresh_tokens.contains_key(&refresh_token_digest("expired-raw")));
    assert_eq!(reloaded.kicked_users.get(&99), Some(&(950, "Displaced".to_string())));
}

#[test]
fn group_e_incremental_sqlite_helpers_cover_zero_and_nonzero_now_paths() {
    let mut conn = open_auth_conn();
    let hash1 = refresh_token_digest("tok-1");
    let rec1 = sample_refresh_record(&hash1, 10, 800, false);

    // last_auth_now_unix == 0 skips prune_in_tx and persists rows cleanly
    persist_minted_refresh_token(&mut conn, &hash1, &rec1, 0).expect("mint now=0");
    persist_revoked_jti(&mut conn, "jti-early", 500, 0).expect("revoke jti now=0");
    persist_kicked_user(&mut conn, 10, 100, "Kick0", &[], &[], 0).expect("kick now=0");
    let meta_cnt: i64 = conn
        .query_row("SELECT COUNT(*) FROM auth_runtime_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(meta_cnt, 0);

    // Revoke refresh token + clear kicked user
    persist_refresh_token_revoked(&conn, &hash1).expect("revoke rt");
    persist_clear_kicked_user(&conn, 10).expect("clear kick");
    let loaded = load_auth_runtime_from_sqlite(&conn, [6u8; 32]).expect("load");
    assert!(loaded.refresh_tokens[&hash1].revoked);
    assert!(!loaded.kicked_users.contains_key(&10));
    assert!(loaded.revoked_jtis.contains("jti-early"));

    // Non-zero now_unix triggers prune_in_tx inside incremental helpers
    let hash2 = refresh_token_digest("tok-2");
    let rec2 = sample_refresh_record(&hash2, 11, 2_000, false);
    persist_minted_refresh_token(&mut conn, &hash2, &rec2, 600).expect("mint with prune");
    persist_revoked_jti(&mut conn, "jti-late", 1_500, 650).expect("revoke jti with prune");
    persist_kicked_user(
        &mut conn,
        11,
        700,
        "AdminBan",
        std::slice::from_ref(&hash2),
        &[("jti-kicked".to_string(), 1_600)],
        700,
    )
    .expect("kick with tokens and jtis");

    let loaded2 = load_auth_runtime_from_sqlite(&conn, [6u8; 32]).expect("load2");
    assert_eq!(loaded2.last_auth_now_unix, 700);
    assert!(!loaded2.revoked_jtis.contains("jti-early")); // exp 500 <= 700 pruned
    assert!(loaded2.revoked_jtis.contains("jti-late"));
    assert!(loaded2.revoked_jtis.contains("jti-kicked"));
    assert!(loaded2.refresh_tokens[&hash2].revoked);
    assert_eq!(loaded2.kicked_users.get(&11), Some(&(700, "AdminBan".to_string())));

    // persist_prune_expired(0) returns Ok(0) immediately without DB writes
    assert_eq!(persist_prune_expired(&mut conn, 0).expect("prune 0"), 0);
}

#[test]
fn group_f_prune_in_tx_enforces_monotonic_max_and_missing_table_errors() {
    // 1. Missing auth_runtime_meta -> error at meta INSERT/REPLACE
    {
        let mut conn = open_auth_conn();
        conn.execute("DROP TABLE auth_runtime_meta", []).unwrap();
        let err = persist_prune_expired(&mut conn, 900).expect_err("missing meta table must err");
        assert!(!err.is_empty());
    }

    // 2. Missing auth_revoked_jtis -> error at JTI DELETE
    {
        let mut conn = open_auth_conn();
        conn.execute("DROP TABLE auth_revoked_jtis", []).unwrap();
        let err = persist_prune_expired(&mut conn, 900).expect_err("missing jti table must err");
        assert!(!err.is_empty());
    }

    // 3. Missing auth_refresh_tokens -> error at refresh DELETE
    {
        let mut conn = open_auth_conn();
        conn.execute("DROP TABLE auth_refresh_tokens", []).unwrap();
        let err =
            persist_prune_expired(&mut conn, 900).expect_err("missing refresh table must err");
        assert!(!err.is_empty());
    }

    // 4. Explicit DB MAX(?1, existing) monotonicity check and asymmetric prune counts
    let mut conn = open_auth_conn();
    conn.execute("INSERT INTO auth_revoked_jtis (jti, expires_at_unix) VALUES ('jti-2000', 2000)", [])
        .unwrap();
    conn.execute("INSERT INTO auth_revoked_jtis (jti, expires_at_unix) VALUES ('jti-2500', 2500)", [])
        .unwrap();
    let scopes = "[]";
    for (h, exp) in [("rt-1999", 1999_i64), ("rt-2000", 2000_i64)] {
        conn.execute(
            "INSERT INTO auth_refresh_tokens (token_hash, user_id, character_id, client_id, scopes_json, expires_at_unix, revoked, last_access_token_jti, last_access_token_exp_unix)
             VALUES (?1, 1, 90000001, 'c', ?2, ?3, 0, 'j', 0)",
            params![h, scopes, exp],
        )
        .unwrap();
    }

    let pruned = persist_prune_expired(&mut conn, 2_000).expect("prune at 2000");
    assert_eq!(pruned, 2); // jti-2000 (<= 2000) + rt-1999 (< 2000); rt-2000 preserved

    // Calling prune with older timestamp 1200 must NEVER regress last_auth_now_unix below 2000
    let pruned2 = persist_prune_expired(&mut conn, 1_200).expect("prune at 1200");
    assert_eq!(pruned2, 0);

    let stored_now: i64 = conn
        .query_row(
            "SELECT meta_value FROM auth_runtime_meta WHERE meta_key = 'last_auth_now_unix'",
            [],
            |r| r.get(0),
        )
        .expect("query last_auth_now_unix");
    assert_eq!(stored_now, 2_000);
}
