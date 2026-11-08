//! Durable SQLite persistence for `AuthRuntimeState` (`#540`):
//! - `auth_revoked_jtis` (pruned via stored `exp` timestamps)
//! - `auth_kicked_users`
//! - `auth_refresh_tokens` (keyed strictly by SHA-256 hex digest; plaintext tokens are never stored)
//! - `auth_runtime_meta`

use crate::accounts::sha256_bytes;
use crate::types::{AuthRuntimeState, SsoRefreshTokenRecord};
use rusqlite::{params, Connection};

/// Computes the canonical lowercase SHA-256 hex digest for a raw SSO refresh token.
pub fn refresh_token_digest(raw_token: &str) -> String {
    let digest = sha256_bytes(raw_token.as_bytes());
    let mut hex = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{b:02x}");
    }
    hex
}

pub fn apply_auth_migration(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS auth_revoked_jtis (
            jti TEXT PRIMARY KEY,
            expires_at_unix INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS auth_kicked_users (
            user_id INTEGER PRIMARY KEY,
            kicked_at_unix INTEGER NOT NULL,
            reason TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS auth_refresh_tokens (
            token_hash TEXT PRIMARY KEY,
            user_id INTEGER NOT NULL,
            character_id INTEGER NOT NULL,
            client_id TEXT NOT NULL,
            scopes_json TEXT NOT NULL,
            expires_at_unix INTEGER NOT NULL,
            revoked INTEGER NOT NULL,
            last_access_token_jti TEXT NOT NULL,
            last_access_token_exp_unix INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS auth_runtime_meta (
            meta_key TEXT PRIMARY KEY,
            meta_value INTEGER NOT NULL
        );",
    )
    .map_err(|e| e.to_string())?;
    let _ = conn.execute(
        "ALTER TABLE auth_refresh_tokens ADD COLUMN last_access_token_exp_unix INTEGER NOT NULL DEFAULT 0",
        [],
    );
    Ok(())
}

/// Prunes expired `revoked_jtis` (and expired `refresh_tokens`) from the in-memory `AuthRuntimeState`
/// using `now_unix` and stored `exp` timestamps. Returns the number of entries pruned.
pub(crate) fn prune_auth_runtime_locked(rt: &mut AuthRuntimeState, now_unix: u64) -> usize {
    if now_unix > rt.last_auth_now_unix {
        rt.last_auth_now_unix = now_unix;
    }
    let cutoff = rt.last_auth_now_unix;
    if cutoff == 0 {
        return 0;
    }
    let expired_jtis: Vec<String> = rt
        .revoked_jti_expiries
        .iter()
        .filter_map(|(jti, &exp)| (exp > 0 && exp <= cutoff).then(|| jti.clone()))
        .collect();
    let mut pruned = expired_jtis.len();
    for jti in expired_jtis {
        rt.revoked_jti_expiries.remove(&jti);
        rt.revoked_jtis.remove(&jti);
    }
    let before_rt = rt.refresh_tokens.len();
    rt.refresh_tokens
        .retain(|_, rec| rec.expires_at_unix == 0 || cutoff <= rec.expires_at_unix);
    pruned += before_rt.saturating_sub(rt.refresh_tokens.len());
    pruned
}

pub(crate) fn load_auth_runtime_from_sqlite(
    conn: &Connection,
    jwt_secret: [u8; 32],
) -> Result<AuthRuntimeState, String> {
    let mut rt = AuthRuntimeState::new(jwt_secret);

    let last_now: u64 = conn
        .query_row(
            "SELECT meta_value FROM auth_runtime_meta WHERE meta_key = 'last_auth_now_unix'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .ok()
        .filter(|&v| v > 0)
        .map(|v| v as u64)
        .unwrap_or(0);
    rt.last_auth_now_unix = last_now;

    let mut expired_jtis = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT jti, expires_at_unix FROM auth_revoked_jtis")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (jti, exp_i64) = r.map_err(|e| e.to_string())?;
            let exp = u64::try_from(exp_i64).unwrap_or(0);
            if last_now > 0 && exp > 0 && exp <= last_now {
                expired_jtis.push(jti);
                continue;
            }
            rt.revoked_jtis.insert(jti.clone());
            if exp > 0 {
                rt.revoked_jti_expiries.insert(jti, exp);
            }
        }
    }
    for jti in expired_jtis {
        let _ = conn.execute("DELETE FROM auth_revoked_jtis WHERE jti = ?1", params![jti]);
    }

    {
        let mut stmt = conn
            .prepare("SELECT user_id, kicked_at_unix, reason FROM auth_kicked_users")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    r.get::<_, i64>(1)? as u64,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (uid, kicked_at, reason) = r.map_err(|e| e.to_string())?;
            rt.kicked_users.insert(uid, (kicked_at, reason));
        }
    }

    let mut expired_refresh_hashes = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT token_hash, user_id, character_id, client_id, scopes_json, expires_at_unix, revoked, last_access_token_jti, last_access_token_exp_unix FROM auth_refresh_tokens",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)? as u64,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)? as u64,
                    r.get::<_, i64>(6)? != 0,
                    r.get::<_, String>(7)?,
                    r.get::<_, i64>(8)? as u64,
                ))
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (
                token_hash,
                user_id,
                character_id,
                client_id,
                scopes_json,
                expires_at_unix,
                revoked,
                last_access_token_jti,
                last_access_token_exp_unix,
            ) = r.map_err(|e| e.to_string())?;
            if last_now > 0 && expires_at_unix > 0 && last_now > expires_at_unix {
                expired_refresh_hashes.push(token_hash);
                continue;
            }
            let scopes: Vec<String> =
                serde_json::from_str(&scopes_json).map_err(|e| e.to_string())?;
            rt.refresh_tokens.insert(
                token_hash.clone(),
                SsoRefreshTokenRecord {
                    refresh_token: token_hash,
                    user_id,
                    character_id,
                    client_id,
                    scopes,
                    expires_at_unix,
                    revoked,
                    last_access_token_jti,
                    last_access_token_exp_unix,
                },
            );
        }
    }
    for h in expired_refresh_hashes {
        let _ = conn.execute(
            "DELETE FROM auth_refresh_tokens WHERE token_hash = ?1",
            params![h],
        );
    }

    Ok(rt)
}

pub(crate) fn replace_auth_runtime_in_sqlite(
    conn: &mut Connection,
    rt: &AuthRuntimeState,
) -> Result<(), String> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    tx.execute_batch(
        "DELETE FROM auth_revoked_jtis;
         DELETE FROM auth_kicked_users;
         DELETE FROM auth_refresh_tokens;
         DELETE FROM auth_runtime_meta;",
    )
    .map_err(|e| e.to_string())?;

    if rt.last_auth_now_unix > 0 {
        tx.execute(
            "INSERT OR REPLACE INTO auth_runtime_meta (meta_key, meta_value) VALUES ('last_auth_now_unix', ?1)",
            params![rt.last_auth_now_unix as i64],
        )
        .map_err(|e| e.to_string())?;
    }

    for jti in &rt.revoked_jtis {
        let exp = rt.revoked_jti_expiries.get(jti).copied().unwrap_or(0);
        if rt.last_auth_now_unix > 0 && exp > 0 && exp <= rt.last_auth_now_unix {
            continue;
        }
        tx.execute(
            "INSERT OR REPLACE INTO auth_revoked_jtis (jti, expires_at_unix) VALUES (?1, ?2)",
            params![jti, exp as i64],
        )
        .map_err(|e| e.to_string())?;
    }

    for (&uid, (kicked_at, reason)) in &rt.kicked_users {
        tx.execute(
            "INSERT OR REPLACE INTO auth_kicked_users (user_id, kicked_at_unix, reason) VALUES (?1, ?2, ?3)",
            params![uid as i64, *kicked_at as i64, reason],
        )
        .map_err(|e| e.to_string())?;
    }

    for (key, rec) in &rt.refresh_tokens {
        if rt.last_auth_now_unix > 0
            && rec.expires_at_unix > 0
            && rt.last_auth_now_unix > rec.expires_at_unix
        {
            continue;
        }
        let token_hash = if key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()) {
            key.clone()
        } else {
            refresh_token_digest(key)
        };
        let scopes_json = serde_json::to_string(&rec.scopes).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT OR REPLACE INTO auth_refresh_tokens (token_hash, user_id, character_id, client_id, scopes_json, expires_at_unix, revoked, last_access_token_jti, last_access_token_exp_unix)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                token_hash,
                rec.user_id as i64,
                rec.character_id,
                rec.client_id,
                scopes_json,
                rec.expires_at_unix as i64,
                i64::from(rec.revoked),
                rec.last_access_token_jti,
                rec.last_access_token_exp_unix as i64,
            ],
        )
        .map_err(|e| e.to_string())?;
    }

    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn persist_minted_refresh_token(
    conn: &mut Connection,
    token_hash: &str,
    rec: &SsoRefreshTokenRecord,
    last_auth_now_unix: u64,
) -> Result<(), String> {
    let scopes_json = serde_json::to_string(&rec.scopes).map_err(|e| e.to_string())?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT OR REPLACE INTO auth_refresh_tokens (token_hash, user_id, character_id, client_id, scopes_json, expires_at_unix, revoked, last_access_token_jti, last_access_token_exp_unix)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            token_hash,
            rec.user_id as i64,
            rec.character_id,
            rec.client_id,
            scopes_json,
            rec.expires_at_unix as i64,
            i64::from(rec.revoked),
            rec.last_access_token_jti,
            rec.last_access_token_exp_unix as i64,
        ],
    )
    .map_err(|e| e.to_string())?;
    if last_auth_now_unix > 0 {
        prune_in_tx(&tx, last_auth_now_unix)?;
    }
    tx.commit().map_err(|e| e.to_string())
}

pub(crate) fn persist_refresh_token_revoked(
    conn: &Connection,
    token_hash: &str,
) -> Result<(), String> {
    conn.execute(
        "UPDATE auth_refresh_tokens SET revoked = 1 WHERE token_hash = ?1",
        params![token_hash],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn persist_revoked_jti(
    conn: &mut Connection,
    jti: &str,
    exp_unix: u64,
    last_auth_now_unix: u64,
) -> Result<(), String> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT OR REPLACE INTO auth_revoked_jtis (jti, expires_at_unix) VALUES (?1, ?2)",
        params![jti, exp_unix as i64],
    )
    .map_err(|e| e.to_string())?;
    if last_auth_now_unix > 0 {
        prune_in_tx(&tx, last_auth_now_unix)?;
    }
    tx.commit().map_err(|e| e.to_string())
}

pub(crate) fn persist_kicked_user(
    conn: &mut Connection,
    user_id: u64,
    kicked_at_unix: u64,
    reason: &str,
    revoked_token_hashes: &[String],
    revoked_jtis: &[(String, u64)],
    last_auth_now_unix: u64,
) -> Result<(), String> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT OR REPLACE INTO auth_kicked_users (user_id, kicked_at_unix, reason) VALUES (?1, ?2, ?3)",
        params![user_id as i64, kicked_at_unix as i64, reason],
    )
    .map_err(|e| e.to_string())?;
    for token_hash in revoked_token_hashes {
        tx.execute(
            "UPDATE auth_refresh_tokens SET revoked = 1 WHERE token_hash = ?1",
            params![token_hash],
        )
        .map_err(|e| e.to_string())?;
    }
    for (jti, exp_unix) in revoked_jtis {
        tx.execute(
            "INSERT OR REPLACE INTO auth_revoked_jtis (jti, expires_at_unix) VALUES (?1, ?2)",
            params![jti, *exp_unix as i64],
        )
        .map_err(|e| e.to_string())?;
    }
    if last_auth_now_unix > 0 {
        prune_in_tx(&tx, last_auth_now_unix)?;
    }
    tx.commit().map_err(|e| e.to_string())
}

pub(crate) fn persist_clear_kicked_user(conn: &Connection, user_id: u64) -> Result<(), String> {
    conn.execute(
        "DELETE FROM auth_kicked_users WHERE user_id = ?1",
        params![user_id as i64],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn persist_prune_expired(conn: &mut Connection, now_unix: u64) -> Result<usize, String> {
    if now_unix == 0 {
        return Ok(0);
    }
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let count = prune_in_tx(&tx, now_unix)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(count)
}

fn prune_in_tx(tx: &rusqlite::Transaction<'_>, now_unix: u64) -> Result<usize, String> {
    tx.execute(
        "INSERT OR REPLACE INTO auth_runtime_meta (meta_key, meta_value)
         VALUES ('last_auth_now_unix', MAX(?1, COALESCE((SELECT meta_value FROM auth_runtime_meta WHERE meta_key = 'last_auth_now_unix'), 0)))",
        params![now_unix as i64],
    )
    .map_err(|e| e.to_string())?;
    let jti_pruned = tx
        .execute(
            "DELETE FROM auth_revoked_jtis WHERE expires_at_unix > 0 AND expires_at_unix <= ?1",
            params![now_unix as i64],
        )
        .map_err(|e| e.to_string())?;
    let rt_pruned = tx
        .execute(
            "DELETE FROM auth_refresh_tokens WHERE expires_at_unix > 0 AND expires_at_unix < ?1",
            params![now_unix as i64],
        )
        .map_err(|e| e.to_string())?;
    Ok(jti_pruned + rt_pruned)
}
