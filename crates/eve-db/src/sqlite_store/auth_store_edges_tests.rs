use super::*;
use crate::types::{AuthRuntimeState, SsoRefreshTokenRecord};
use rusqlite::{Connection, OpenFlags};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

fn temp_sqlite_path(tag: &str) -> std::path::PathBuf {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "evomu_auth_store_edges_{tag}_{}_{seq}.sqlite",
        std::process::id()
    ))
}

fn mem_migrated() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory sqlite");
    apply_auth_migration(&conn).expect("apply_auth_migration");
    conn
}

fn sample_refresh_rec(
    raw_or_hash: &str,
    user_id: u64,
    expires_at_unix: u64,
    revoked: bool,
) -> SsoRefreshTokenRecord {
    SsoRefreshTokenRecord {
        refresh_token: raw_or_hash.to_string(),
        user_id,
        character_id: 90_001,
        client_id: "eve-client".to_string(),
        scopes: vec!["publicData".to_string(), "esi-skills.read_skills.v1".to_string()],
        expires_at_unix,
        revoked,
        last_access_token_jti: "jti-1".to_string(),
        last_access_token_exp_unix: expires_at_unix,
    }
}

#[test]
fn refresh_token_digest_and_migration_edge_cases() {
    let d_empty = refresh_token_digest("");
    let d_a = refresh_token_digest("tok-a");
    let d_b = refresh_token_digest("tok-b");
    assert_eq!(d_empty.len(), 64);
    assert!(d_empty.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    assert_eq!(d_a, refresh_token_digest("tok-a"));
    assert_ne!(d_a, d_b);
    assert_ne!(d_empty, d_a);

    // Legacy auth_refresh_tokens table missing last_access_token_exp_unix gets migrated cleanly.
    let conn = Connection::open_in_memory().expect("in-memory");
    conn.execute_batch(
        "CREATE TABLE auth_refresh_tokens (
            token_hash TEXT PRIMARY KEY,
            user_id INTEGER NOT NULL,
            character_id INTEGER NOT NULL,
            client_id TEXT NOT NULL,
            scopes_json TEXT NOT NULL,
            expires_at_unix INTEGER NOT NULL,
            revoked INTEGER NOT NULL,
            last_access_token_jti TEXT NOT NULL DEFAULT ''
        );",
    )
    .expect("create legacy table");
    apply_auth_migration(&conn).expect("legacy migration");
    // Re-applying is idempotent.
    apply_auth_migration(&conn).expect("idempotent migration");
    let mut stmt = conn
        .prepare("PRAGMA table_info(auth_refresh_tokens)")
        .expect("pragma");
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .expect("query cols")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect cols");
    assert!(cols.contains(&"last_access_token_exp_unix".to_string()));

    // Read-only database triggers migration failure.
    let ro_path = temp_sqlite_path("ro_mig");
    {
        let _ = Connection::open(&ro_path).expect("create file");
    }
    let ro_conn =
        Connection::open_with_flags(&ro_path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open ro");
    assert!(apply_auth_migration(&ro_conn).is_err());
    drop(ro_conn);
    let _ = std::fs::remove_file(&ro_path);
}

#[test]
fn prune_auth_runtime_locked_boundaries_and_monotonic_cutoff() {
    let mut rt = AuthRuntimeState::new([7u8; 32]);
    rt.revoked_jtis.insert("perm".to_string());
    rt.revoked_jti_expiries.insert("perm".to_string(), 0);
    rt.revoked_jtis.insert("exact".to_string());
    rt.revoked_jti_expiries.insert("exact".to_string(), 500);
    rt.revoked_jtis.insert("future".to_string());
    rt.revoked_jti_expiries.insert("future".to_string(), 501);

    let tok_perm = refresh_token_digest("perm");
    let tok_exact = refresh_token_digest("exact");
    let tok_past = refresh_token_digest("past");
    rt.refresh_tokens
        .insert(tok_perm.clone(), sample_refresh_rec(&tok_perm, 1, 0, false));
    rt.refresh_tokens
        .insert(tok_exact.clone(), sample_refresh_rec(&tok_exact, 2, 500, false));
    rt.refresh_tokens
        .insert(tok_past.clone(), sample_refresh_rec(&tok_past, 3, 499, false));

    // When both last_auth_now_unix and now_unix are 0, nothing is pruned.
    assert_eq!(prune_auth_runtime_locked(&mut rt, 0), 0);
    assert_eq!(rt.last_auth_now_unix, 0);

    // Advance to 500: JTI exp <= 500 pruned; refresh token exp < 500 pruned (exact == 500 kept).
    assert_eq!(prune_auth_runtime_locked(&mut rt, 500), 2);
    assert_eq!(rt.last_auth_now_unix, 500);
    assert!(rt.revoked_jtis.contains("perm"));
    assert!(rt.revoked_jtis.contains("future"));
    assert!(!rt.revoked_jtis.contains("exact"));
    assert!(rt.refresh_tokens.contains_key(&tok_perm));
    assert!(rt.refresh_tokens.contains_key(&tok_exact));
    assert!(!rt.refresh_tokens.contains_key(&tok_past));

    // Calling with smaller now_unix does not regress last_auth_now_unix.
    assert_eq!(prune_auth_runtime_locked(&mut rt, 100), 0);
    assert_eq!(rt.last_auth_now_unix, 500);
}

#[test]
fn load_auth_runtime_pruning_coercion_and_error_paths() {
    let conn = mem_migrated();
    // Negative meta clamps to 0; test negative column coercions + valid rows.
    conn.execute(
        "INSERT INTO auth_runtime_meta (meta_key, meta_value) VALUES ('last_auth_now_unix', -42)",
        [],
    )
    .expect("meta");
    conn.execute(
        "INSERT INTO auth_revoked_jtis (jti, expires_at_unix) VALUES ('neg-jti', -5)",
        [],
    )
    .expect("neg jti");
    conn.execute(
        "INSERT INTO auth_kicked_users (user_id, kicked_at_unix, reason) VALUES (10, -3, 'kicked')",
        [],
    )
    .expect("kicked");
    let tok_hash = refresh_token_digest("raw-1");
    conn.execute(
        "INSERT INTO auth_refresh_tokens (token_hash, user_id, character_id, client_id, scopes_json, expires_at_unix, revoked, last_access_token_jti, last_access_token_exp_unix)
         VALUES (?1, -1, -2, 'cid', '[\"scopeA\"]', -9, 1, 'jti-x', -1)",
        params![tok_hash],
    )
    .expect("tok");

    let rt = load_auth_runtime_from_sqlite(&conn, [3u8; 32]).expect("load with clamped values");
    assert_eq!(rt.last_auth_now_unix, 0);
    assert!(rt.revoked_jtis.contains("neg-jti"));
    assert_eq!(rt.revoked_jti_expiries.get("neg-jti"), None);
    assert_eq!(
        rt.kicked_users.get(&10),
        Some(&((-3_i64) as u64, "kicked".to_string()))
    );
    let loaded_tok = rt.refresh_tokens.get(&tok_hash).expect("tok present");
    assert_eq!(loaded_tok.user_id, (-1_i64) as u64);
    assert_eq!(loaded_tok.character_id, -2);
    assert_eq!(loaded_tok.expires_at_unix, (-9_i64) as u64);
    assert!(loaded_tok.revoked);
    assert_eq!(loaded_tok.last_access_token_exp_unix, (-1_i64) as u64);

    // Now set positive last_auth_now_unix and verify stale rows are pruned on load & deleted from SQLite.
    let conn2 = mem_migrated();
    conn2
        .execute(
            "INSERT INTO auth_runtime_meta (meta_key, meta_value) VALUES ('last_auth_now_unix', 1000)",
            [],
        )
        .expect("meta");
    conn2
        .execute(
            "INSERT INTO auth_revoked_jtis (jti, expires_at_unix) VALUES ('exp-jti', 1000), ('live-jti', 1001), ('perm-jti', 0)",
            [],
        )
        .expect("jtis");
    let h_exp = refresh_token_digest("exp");
    let h_edge = refresh_token_digest("edge");
    let h_perm = refresh_token_digest("perm");
    conn2
        .execute(
            "INSERT INTO auth_refresh_tokens (token_hash, user_id, character_id, client_id, scopes_json, expires_at_unix, revoked, last_access_token_jti, last_access_token_exp_unix)
             VALUES (?1, 1, 2, 'c', '[]', 999, 0, 'j1', 999),
                    (?2, 1, 2, 'c', '[]', 1000, 0, 'j2', 1000),
                    (?3, 1, 2, 'c', '[]', 0, 0, 'j3', 0)",
            params![h_exp, h_edge, h_perm],
        )
        .expect("tokens");

    let rt2 = load_auth_runtime_from_sqlite(&conn2, [4u8; 32]).expect("load pruned");
    assert_eq!(rt2.last_auth_now_unix, 1000);
    assert!(!rt2.revoked_jtis.contains("exp-jti"));
    assert!(rt2.revoked_jtis.contains("live-jti"));
    assert!(rt2.revoked_jtis.contains("perm-jti"));
    assert!(!rt2.refresh_tokens.contains_key(&h_exp));
    assert!(rt2.refresh_tokens.contains_key(&h_edge));
    assert!(rt2.refresh_tokens.contains_key(&h_perm));
    let remaining_jtis: i64 = conn2
        .query_row("SELECT COUNT(*) FROM auth_revoked_jtis", [], |r| r.get(0))
        .expect("count jtis");
    let remaining_toks: i64 = conn2
        .query_row("SELECT COUNT(*) FROM auth_refresh_tokens", [], |r| r.get(0))
        .expect("count toks");
    assert_eq!(remaining_jtis, 2);
    assert_eq!(remaining_toks, 2);

    // Invalid JSON in scopes_json triggers Err.
    conn2
        .execute(
            "UPDATE auth_refresh_tokens SET scopes_json = '{not-json' WHERE token_hash = ?1",
            params![h_edge],
        )
        .expect("corrupt json");
    assert!(load_auth_runtime_from_sqlite(&conn2, [4u8; 32]).is_err());

    // Missing tables trigger Err at each query phase.
    for drop_sql in [
        "DROP TABLE auth_refresh_tokens",
        "DROP TABLE auth_kicked_users",
        "DROP TABLE auth_revoked_jtis",
    ] {
        let c = mem_migrated();
        c.execute(drop_sql, []).expect("drop table");
        assert!(load_auth_runtime_from_sqlite(&c, [1u8; 32]).is_err());
    }
}

#[test]
fn replace_and_incremental_persist_helpers_cover_all_branches() {
    let mut conn = mem_migrated();

    // replace_auth_runtime_in_sqlite with last_auth_now_unix == 0 stores meta removal & hashes non-hex key.
    let mut rt = AuthRuntimeState::new([9u8; 32]);
    rt.last_auth_now_unix = 0;
    rt.revoked_jtis.insert("j-zero".to_string());
    rt.revoked_jti_expiries.insert("j-zero".to_string(), 0);
    rt.kicked_users
        .insert(42, (111, "admin kick".to_string()));
    rt.refresh_tokens
        .insert("raw-token-key".to_string(), sample_refresh_rec("raw-token-key", 42, 0, false));
    let hex_key = refresh_token_digest("already-hashed");
    rt.refresh_tokens
        .insert(hex_key.clone(), sample_refresh_rec(&hex_key, 43, 500, true));

    replace_auth_runtime_in_sqlite(&mut conn, &rt).expect("replace at 0");
    let meta_cnt: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM auth_runtime_meta WHERE meta_key = 'last_auth_now_unix'",
            [],
            |r| r.get(0),
        )
        .expect("meta cnt");
    assert_eq!(meta_cnt, 0);

    let reloaded = load_auth_runtime_from_sqlite(&conn, [9u8; 32]).expect("reload");
    assert!(reloaded
        .refresh_tokens
        .contains_key(&refresh_token_digest("raw-token-key")));
    assert!(reloaded.refresh_tokens.contains_key(&hex_key));

    // replace_auth_runtime_in_sqlite with last_auth_now_unix > 0 skips expired entries & writes meta.
    rt.last_auth_now_unix = 600;
    rt.revoked_jtis.insert("j-expired".to_string());
    rt.revoked_jti_expiries.insert("j-expired".to_string(), 600);
    rt.revoked_jtis.insert("j-live".to_string());
    rt.revoked_jti_expiries.insert("j-live".to_string(), 700);
    replace_auth_runtime_in_sqlite(&mut conn, &rt).expect("replace at 600");
    let reloaded2 = load_auth_runtime_from_sqlite(&conn, [9u8; 32]).expect("reload 2");
    assert_eq!(reloaded2.last_auth_now_unix, 600);
    assert!(!reloaded2.revoked_jtis.contains("j-expired"));
    assert!(reloaded2.revoked_jtis.contains("j-live"));
    assert!(!reloaded2.refresh_tokens.contains_key(&hex_key));

    // Incremental persistence calls with last_auth_now_unix == 0 and > 0.
    let mut conn_inc = mem_migrated();
    let h1 = refresh_token_digest("inc-1");
    let h2 = refresh_token_digest("inc-2");
    persist_minted_refresh_token(
        &mut conn_inc,
        &h1,
        &sample_refresh_rec(&h1, 7, 1500, false),
        0,
    )
    .expect("mint at 0");
    persist_minted_refresh_token(
        &mut conn_inc,
        &h2,
        &sample_refresh_rec(&h2, 7, 2000, false),
        1200,
    )
    .expect("mint at 1200");
    persist_refresh_token_revoked(&conn_inc, &h1).expect("revoke h1");
    persist_revoked_jti(&mut conn_inc, "jti-a", 1300, 0).expect("jti at 0");
    persist_revoked_jti(&mut conn_inc, "jti-b", 1200, 1250).expect("jti at 1250");
    persist_kicked_user(
        &mut conn_inc,
        7,
        1250,
        "gm kick",
        &[h2.clone()],
        &[("jti-k1".to_string(), 0), ("jti-k2".to_string(), 1400)],
        0,
    )
    .expect("kick at 0");
    persist_kicked_user(&mut conn_inc, 8, 1260, "gm kick 2", &[], &[], 1260)
        .expect("kick at 1260");
    persist_clear_kicked_user(&conn_inc, 8).expect("clear kick 8");

    // persist_prune_expired(0) returns 0; > 0 prunes expired JTI & refresh tokens while preserving edge/perm.
    assert_eq!(persist_prune_expired(&mut conn_inc, 0).expect("prune 0"), 0);
    let h_edge = refresh_token_digest("inc-edge");
    let h_perm = refresh_token_digest("inc-perm");
    persist_minted_refresh_token(
        &mut conn_inc,
        &h_edge,
        &sample_refresh_rec(&h_edge, 9, 1501, false),
        0,
    )
    .expect("mint edge");
    persist_minted_refresh_token(
        &mut conn_inc,
        &h_perm,
        &sample_refresh_rec(&h_perm, 9, 0, false),
        0,
    )
    .expect("mint perm");
    let pruned = persist_prune_expired(&mut conn_inc, 1501).expect("prune 1501");
    // Pruned JTIs: jti-a (1300<=1501), jti-k2 (1400<=1501) -> 2 (jti-b was already pruned at 1250!)
    // Pruned refresh tokens: h1 (1500<1501) -> 1; h2 (2000), h_edge (1501), h_perm (0) kept.
    assert_eq!(pruned, 3);

    let state = load_auth_runtime_from_sqlite(&conn_inc, [1u8; 32]).expect("final load");
    assert_eq!(state.last_auth_now_unix, 1501);
    assert!(state.kicked_users.contains_key(&7));
    assert!(!state.kicked_users.contains_key(&8));
    assert!(state.refresh_tokens.get(&h2).expect("h2").revoked);
    assert!(state.revoked_jtis.contains("jti-k1"));
    assert!(!state.revoked_jtis.contains("jti-k2"));

    // Error propagation on missing tables for all mutating persistence helpers.
    conn_inc
        .execute_batch(
            "DROP TABLE auth_refresh_tokens;
             DROP TABLE auth_revoked_jtis;
             DROP TABLE auth_kicked_users;
             DROP TABLE auth_runtime_meta;",
        )
        .expect("drop all");
    assert!(replace_auth_runtime_in_sqlite(&mut conn_inc, &rt).is_err());
    assert!(persist_minted_refresh_token(
        &mut conn_inc,
        &h1,
        &sample_refresh_rec(&h1, 1, 10, false),
        10,
    )
    .is_err());
    assert!(persist_refresh_token_revoked(&conn_inc, &h1).is_err());
    assert!(persist_revoked_jti(&mut conn_inc, "j", 10, 10).is_err());
    assert!(persist_kicked_user(&mut conn_inc, 1, 10, "r", &[], &[], 10).is_err());
    assert!(persist_clear_kicked_user(&conn_inc, 1).is_err());
    assert!(persist_prune_expired(&mut conn_inc, 10).is_err());
}
