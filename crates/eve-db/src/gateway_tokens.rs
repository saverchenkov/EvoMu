//! Quasar gRPC gateway JWT issuance and strict cryptographic verification (`R6-T4-01`).
//!
//! Eliminates all hardcoded `"dev"` JWT secret acceptance while supporting both:
//! - Deployment OAuth2/PKCE SSO HS256 JWTs (`verify_sso_access_token`, full role set), and
//! - Local-dev gateway HS256 JWTs signed strictly against the server's own secret
//!   (`AuthRuntime::jwt_signing_secret`) with mandatory `exp`, strict `sub` ownership validation,
//!   zero injected privilege claims, and suppressed `ROLE_GM_MASK` elevation.

#![forbid(unsafe_code)]

use crate::accounts::{
    base64url_decode_no_pad, base64url_encode_no_pad, constant_time_eq, hmac_sha256, sha256_bytes,
};
use crate::persistence::WorldRepository;
use crate::sqlite_store;
use crate::types::{
    DbError, PkceAuthCode, SsoRefreshTokenRecord, SsoTokenBundle, SsoTokenClaims,
};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use crate::lock_rank::{RankedMutex, RANK_LEAF};
use std::sync::{Arc, LazyLock};

struct DedicatedAuthConn {
    conn: RankedMutex<rusqlite::Connection, RANK_LEAF>,
    force_fail: AtomicBool,
}

static AUTH_SQLITE_CONNS: LazyLock<RankedMutex<HashMap<u64, Arc<DedicatedAuthConn>>, RANK_LEAF>> =
    LazyLock::new(|| RankedMutex::new(HashMap::new()));

/// Normalizes IPv4-mapped IPv6 addresses (`::ffff:a.b.c.d`) to canonical `IpAddr::V4` so dual-stack
/// sockets on `:26000` and `:26005` compare identically (`#472`).
pub fn normalize_peer_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => IpAddr::V4(v4),
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
    }
}

static GATEWAY_JWT_COUNTER: AtomicU64 = AtomicU64::new(1);

const FORBIDDEN_PRIVILEGE_CLAIM_KEYS: &[&str] = &[
    "role",
    "roles",
    "admin",
    "gm",
    "is_admin",
    "is_gm",
    "service_roles",
    "service_role_mask",
    "corp_role",
    "corp_roles",
    "permissions",
    "scope",
    "scopes",
];

/// Verified bearer token identity for Quasar gRPC sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuasarVerifiedToken {
    pub user_id: u64,
    pub character_id: Option<i64>,
    /// `true` for full OAuth2/PKCE SSO access tokens; `false` for local-dev gateway tokens.
    /// Non-SSO gateway tokens never inherit GM/Admin service role bits over Quasar gRPC.
    pub is_full_sso: bool,
}

fn is_privileged_scope_token(scope: &str) -> bool {
    let lower = scope.to_ascii_lowercase();
    scope.is_empty()
        || scope.chars().any(char::is_whitespace)
        || matches!(lower.as_str(), "admin" | "gm" | "role" | "roles" | "superuser" | "root")
        || lower.starts_with("gm:")
        || lower.starts_with("admin:")
        || lower.starts_with("role:")
}

pub(crate) fn payload_contains_forbidden_privilege_claim(
    obj: &Map<String, Value>,
    allow_sso_scp: bool,
) -> bool {
    if FORBIDDEN_PRIVILEGE_CLAIM_KEYS
        .iter()
        .any(|key| obj.contains_key(*key))
    {
        return true;
    }
    if let Some(scp) = obj.get("scp") {
        if !allow_sso_scp {
            return true;
        }
        let Some(arr) = scp.as_array() else {
            return true;
        };
        if arr
            .iter()
            .any(|v| v.as_str().is_none_or(is_privileged_scope_token))
        {
            return true;
        }
    }
    false
}

fn decode_canonical_base64url(segment: &str) -> Result<Vec<u8>, DbError> {
    if segment.is_empty()
        || !segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(DbError::InvalidCredentials);
    }
    let decoded = base64url_decode_no_pad(segment).map_err(|_| DbError::InvalidCredentials)?;
    if base64url_encode_no_pad(&decoded) != segment {
        return Err(DbError::InvalidCredentials);
    }
    Ok(decoded)
}

fn parse_strict_positive_u64(raw: &str) -> Result<u64, DbError> {
    let val = raw.parse::<u64>().map_err(|_| DbError::InvalidCredentials)?;
    if val == 0 || val.to_string() != raw {
        return Err(DbError::InvalidCredentials);
    }
    Ok(val)
}

fn parse_strict_positive_i64(raw: &str) -> Result<i64, DbError> {
    let val = raw.parse::<i64>().map_err(|_| DbError::InvalidCredentials)?;
    if val <= 0 || val.to_string() != raw {
        return Err(DbError::InvalidCredentials);
    }
    Ok(val)
}

const LOCAL_DEV_JWT_CLOCK_SKEW_SECS: u64 = 60;

fn parse_jwt_timestamp_u64(val: &Value) -> Option<u64> {
    val.as_u64().or_else(|| {
        val.as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0 && *v <= u64::MAX as f64)
            .map(|v| v as u64)
    })
}

impl WorldRepository {
    /// Initializes and configures the dedicated SQLite connection for `AuthRuntimeState` persistence
    /// (`PRAGMA busy_timeout = 5000`, `journal_mode = WAL`, `synchronous = NORMAL`) (`#838`, `#879`, `#907`).
    pub fn ensure_auth_sqlite_connection(&self) -> Result<(), String> {
        let _ = self.get_or_init_auth_sqlite_conn()?;
        Ok(())
    }

    fn get_or_init_auth_sqlite_conn(&self) -> Result<Arc<DedicatedAuthConn>, String> {
        {
            let map = AUTH_SQLITE_CONNS.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = map.get(&self.instance_id) {
                return Ok(Arc::clone(existing));
            }
        }
        let active_sqlite_path = self.sqlite_path.as_ref().filter(|p| p.exists());
        if let Ok(primary) = self.sqlite_conn.try_lock() {
            let _ = primary.execute_batch("PRAGMA busy_timeout = 5000; PRAGMA synchronous = NORMAL;");
            if active_sqlite_path.is_some() {
                let _ = primary.execute_batch("PRAGMA journal_mode = WAL;");
            }
        }
        let conn = if let Some(path) = active_sqlite_path {
            let c = rusqlite::Connection::open(path).map_err(|e| e.to_string())?;
            c.execute_batch(
                "PRAGMA busy_timeout = 5000;
                 PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA foreign_keys = ON;",
            )
            .map_err(|e| e.to_string())?;
            sqlite_store::apply_auth_migration(&c)?;
            c
        } else {
            let mut c = rusqlite::Connection::open_in_memory().map_err(|e| e.to_string())?;
            c.execute_batch(
                "PRAGMA busy_timeout = 5000;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA foreign_keys = ON;",
            )
            .map_err(|e| e.to_string())?;
            sqlite_store::apply_migrations(&c)?;
            if let Ok(rt) = self.auth_runtime.try_read() {
                let _ = sqlite_store::save_jwt_secret(&c, &rt.jwt_signing_secret);
                let _ = sqlite_store::replace_auth_runtime_in_sqlite(&mut c, &rt);
            }
            c
        };
        let entry = Arc::new(DedicatedAuthConn {
            conn: RankedMutex::new(conn),
            force_fail: AtomicBool::new(false),
        });
        let mut map = AUTH_SQLITE_CONNS.lock().unwrap_or_else(|e| e.into_inner());
        Ok(Arc::clone(
            map.entry(self.instance_id)
                .or_insert_with(|| Arc::clone(&entry)),
        ))
    }

    /// Executes an auth persistence operation against the dedicated auth SQLite connection (`#838`, `#879`, `#907`).
    /// Callers MUST NOT hold `self.auth_runtime` when invoking this helper.
    pub fn with_auth_sqlite_conn<T>(
        &self,
        f: impl FnOnce(&mut rusqlite::Connection) -> Result<T, String>,
    ) -> Result<T, String> {
        let entry = self.get_or_init_auth_sqlite_conn()?;
        if entry.force_fail.load(Ordering::SeqCst) {
            return Err("Injected auth SQLite persistence failure".to_string());
        }
        let mut conn = entry.conn.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut conn)
    }

    /// Enables or disables deterministic auth SQLite write failure injection for testing fail-closed semantics (`#879`).
    pub fn set_auth_sqlite_fault_for_test(&self, fail: bool) {
        if let Ok(entry) = self.get_or_init_auth_sqlite_conn() {
            entry.force_fail.store(fail, Ordering::SeqCst);
        }
    }

    /// Mints a short-lived local-dev gateway HS256 JWT signed with the server's active
    /// `jwt_signing_secret` for authenticated local `EVE.app` and dev-tool connections.
    pub fn issue_gateway_dev_token(
        &self,
        user_id: u64,
        character_id: Option<i64>,
        tenant: &str,
        exp_unix: u64,
        now_unix: u64,
    ) -> Result<String, DbError> {
        if user_id == 0 || exp_unix <= now_unix {
            return Err(DbError::InvalidCredentials);
        }
        {
            let guard = self.read_state();
            let acc = guard
                .accounts
                .values()
                .find(|a| a.user_id == user_id)
                .ok_or(DbError::InvalidCredentials)?;
            acc.check_active_restriction(now_unix)?;
            if let Some(cid) = character_id {
                if cid <= 0
                    || !guard
                        .characters
                        .get(&cid)
                        .is_some_and(|c| c.user_id == user_id)
                {
                    return Err(DbError::InvalidCredentials);
                }
            }
        }
        let sub = match character_id {
            Some(cid) => format!("CHARACTER:EVE:{cid}"),
            None => format!("USER:EVE:{user_id}"),
        };
        let header_json = serde_json::json!({
            "alg": "HS256",
            "typ": "JWT",
        });
        let seq = GATEWAY_JWT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let jti = format!("gwdev-{now_unix}-{user_id}-{seq}");
        let mut payload = serde_json::Map::new();
        payload.insert("iss".to_string(), Value::String("https://login.eveonline.com".into()));
        payload.insert("sub".to_string(), Value::String(sub));
        payload.insert("iat".to_string(), Value::Number(now_unix.into()));
        payload.insert("exp".to_string(), Value::Number(exp_unix.into()));
        payload.insert("jti".to_string(), Value::String(jti));
        if !tenant.is_empty() {
            payload.insert("tenant".to_string(), Value::String(tenant.to_string()));
        }
        let header_bytes =
            serde_json::to_vec(&header_json).map_err(|_| DbError::InvalidCredentials)?;
        let payload_bytes =
            serde_json::to_vec(&Value::Object(payload)).map_err(|_| DbError::InvalidCredentials)?;
        let header_b64 = base64url_encode_no_pad(&header_bytes);
        let payload_b64 = base64url_encode_no_pad(&payload_bytes);
        let signing_input = format!("{header_b64}.{payload_b64}");
        let rt = self.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
        let sig = hmac_sha256(&rt.jwt_signing_secret, signing_input.as_bytes());
        let sig_b64 = base64url_encode_no_pad(&sig);
        Ok(format!("{signing_input}.{sig_b64}"))
    }

    /// Verifies a Quasar gRPC bearer token against the server's signing secret.
    ///
    /// Accepts either a full OAuth2/PKCE SSO access token or a server-signed local-dev gateway
    /// token with mandatory `exp`, strict HS256 verification, zero injected privilege claims,
    /// and active account/character ownership validation.
    pub fn verify_quasar_bearer_jwt(
        &self,
        token: &str,
        now_unix: u64,
    ) -> Result<QuasarVerifiedToken, DbError> {
        if let Ok(claims) = self.verify_sso_access_token(token, now_unix) {
            if claims.character_id > 0 {
                let guard = self.read_state();
                let valid_char = guard
                    .characters
                    .get(&claims.character_id)
                    .is_some_and(|c| c.user_id == claims.user_id);
                if valid_char {
                    return Ok(QuasarVerifiedToken {
                        user_id: claims.user_id,
                        character_id: Some(claims.character_id),
                        is_full_sso: true,
                    });
                }
            } else if claims.character_id == 0 && claims.user_id > 0 {
                return Ok(QuasarVerifiedToken {
                    user_id: claims.user_id,
                    character_id: None,
                    is_full_sso: true,
                });
            }
            return Err(DbError::InvalidCredentials);
        }

        let mut parts = token.split('.');
        let (Some(header_b64), Some(payload_b64), Some(sig_b64), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(DbError::InvalidCredentials);
        };
        if header_b64.is_empty() || payload_b64.is_empty() || sig_b64.is_empty() {
            return Err(DbError::InvalidCredentials);
        }

        let header_bytes = decode_canonical_base64url(header_b64)?;
        let header_val: Value =
            serde_json::from_slice(&header_bytes).map_err(|_| DbError::InvalidCredentials)?;
        let header_obj = header_val
            .as_object()
            .ok_or(DbError::InvalidCredentials)?;
        if header_obj.get("alg").and_then(|v| v.as_str()) != Some("HS256")
            || header_obj.contains_key("crit")
        {
            return Err(DbError::InvalidCredentials);
        }

        let payload_bytes = decode_canonical_base64url(payload_b64)?;
        let _ = decode_canonical_base64url(sig_b64)?;

        let signing_input = format!("{header_b64}.{payload_b64}");
        let payload_val: Value =
            serde_json::from_slice(&payload_bytes).map_err(|_| DbError::InvalidCredentials)?;
        let payload_obj = payload_val
            .as_object()
            .ok_or(DbError::InvalidCredentials)?;

        if payload_contains_forbidden_privilege_claim(payload_obj, false) {
            return Err(DbError::InvalidCredentials);
        }

        let exp = payload_obj
            .get("exp")
            .and_then(|v| v.as_u64())
            .filter(|&e| e > 0)
            .ok_or(DbError::InvalidCredentials)?;
        if now_unix >= exp {
            return Err(DbError::InvalidCredentials);
        }
        if let Some(nbf_val) = payload_obj.get("nbf") {
            let nbf = nbf_val.as_u64().ok_or(DbError::InvalidCredentials)?;
            if now_unix < nbf {
                return Err(DbError::InvalidCredentials);
            }
        }
        let iat_opt = match payload_obj.get("iat") {
            Some(v) => {
                let iat = v.as_u64().ok_or(DbError::InvalidCredentials)?;
                if iat > now_unix {
                    return Err(DbError::InvalidCredentials);
                }
                Some(iat)
            }
            None => None,
        };
        let jti_opt = match payload_obj.get("jti") {
            Some(v) => {
                let s = v
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or(DbError::InvalidCredentials)?;
                Some(s)
            }
            None => None,
        };

        let sub = payload_obj
            .get("sub")
            .and_then(|v| v.as_str())
            .ok_or(DbError::InvalidCredentials)?;

        let (resolved_user_id, token_char_id): (u64, Option<i64>) = {
            let guard = self.read_state();
            let (uid, cid_opt) = if let Some(rest) = sub.strip_prefix("USER:EVE:") {
                let uid = parse_strict_positive_u64(rest)?;
                (uid, None)
            } else if let Some(rest) = sub.strip_prefix("CHARACTER:EVE:") {
                let cid = parse_strict_positive_i64(rest)?;
                let ch = guard
                    .characters
                    .get(&cid)
                    .filter(|c| c.user_id > 0)
                    .ok_or(DbError::InvalidCredentials)?;
                (ch.user_id, Some(cid))
            } else {
                return Err(DbError::InvalidCredentials);
            };

            if let Some(claimed_uid_val) = payload_obj.get("user_id").or_else(|| payload_obj.get("uid")) {
                let claimed_uid = claimed_uid_val
                    .as_u64()
                    .ok_or(DbError::InvalidCredentials)?;
                if claimed_uid != uid {
                    return Err(DbError::InvalidCredentials);
                }
            }
            if let Some(claimed_cid_val) = payload_obj
                .get("character_id")
                .or_else(|| payload_obj.get("cid"))
            {
                let claimed_cid = claimed_cid_val
                    .as_i64()
                    .filter(|&c| c > 0)
                    .ok_or(DbError::InvalidCredentials)?;
                if let Some(sub_cid) = cid_opt {
                    if claimed_cid != sub_cid {
                        return Err(DbError::InvalidCredentials);
                    }
                }
                if !guard
                    .characters
                    .get(&claimed_cid)
                    .is_some_and(|c| c.user_id == uid)
                {
                    return Err(DbError::InvalidCredentials);
                }
            }

            let acc = guard
                .accounts
                .values()
                .find(|a| a.user_id == uid)
                .ok_or(DbError::InvalidCredentials)?;
            acc.check_active_restriction(now_unix)?;
            drop(guard);

            let rt = self.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
            let expected_sig = hmac_sha256(&rt.jwt_signing_secret, signing_input.as_bytes());
            let expected_sig_b64 = base64url_encode_no_pad(&expected_sig);
            if !constant_time_eq(sig_b64.as_bytes(), expected_sig_b64.as_bytes()) {
                return Err(DbError::InvalidCredentials);
            }
            if let Some(jti) = jti_opt {
                if rt.revoked_jtis.contains(jti) {
                    return Err(DbError::InvalidCredentials);
                }
            }
            if let Some((kicked_at, _)) = rt.kicked_users.get(&uid) {
                if iat_opt.is_none_or(|iat| *kicked_at >= iat) {
                    return Err(DbError::InvalidCredentials);
                }
            }

            (uid, cid_opt)
        };

        Ok(QuasarVerifiedToken {
            user_id: resolved_user_id,
            character_id: token_char_id,
            is_full_sso: false,
        })
    }

    /// Verifies a V24.01 `/server:` client-fabricated local-dev JWT (`fabricate_local_dev_user_token`,
    /// signed with HS256 `"dev"`) ONLY when an active, non-revoked `:26000` `OnlineSession` exists for
    /// `uid` from the exact same normalized `peer_ip` (`#472`). Returned tokens always set
    /// `is_full_sso: false` so `ROLE_GM_MASK` is stripped by the gRPC authenticator.
    pub fn verify_local_dev_client_jwt(
        &self,
        token: &str,
        peer_ip: IpAddr,
        now_unix: u64,
    ) -> Result<QuasarVerifiedToken, DbError> {
        let norm_peer_ip = normalize_peer_ip(peer_ip);
        let mut parts = token.split('.');
        let (Some(header_b64), Some(payload_b64), Some(sig_b64), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(DbError::InvalidCredentials);
        };
        if header_b64.is_empty() || payload_b64.is_empty() || sig_b64.is_empty() {
            return Err(DbError::InvalidCredentials);
        }

        let header_bytes = decode_canonical_base64url(header_b64)?;
        let header_val: Value =
            serde_json::from_slice(&header_bytes).map_err(|_| DbError::InvalidCredentials)?;
        let header_obj = header_val
            .as_object()
            .ok_or(DbError::InvalidCredentials)?;
        if header_obj.get("alg").and_then(|v| v.as_str()) != Some("HS256")
            || header_obj.contains_key("crit")
        {
            return Err(DbError::InvalidCredentials);
        }

        let payload_bytes = decode_canonical_base64url(payload_b64)?;
        let _ = decode_canonical_base64url(sig_b64)?;
        let signing_input = format!("{header_b64}.{payload_b64}");
        let expected_sig = hmac_sha256(b"dev", signing_input.as_bytes());
        let expected_sig_b64 = base64url_encode_no_pad(&expected_sig);
        if !constant_time_eq(sig_b64.as_bytes(), expected_sig_b64.as_bytes()) {
            return Err(DbError::InvalidCredentials);
        }

        let payload_val: Value =
            serde_json::from_slice(&payload_bytes).map_err(|_| DbError::InvalidCredentials)?;
        let payload_obj = payload_val
            .as_object()
            .ok_or(DbError::InvalidCredentials)?;
        if payload_contains_forbidden_privilege_claim(payload_obj, false) {
            return Err(DbError::InvalidCredentials);
        }
        if let Some(exp_val) = payload_obj.get("exp") {
            let exp = parse_jwt_timestamp_u64(exp_val)
                .filter(|&e| e > 0)
                .ok_or(DbError::InvalidCredentials)?;
            if now_unix >= exp {
                return Err(DbError::InvalidCredentials);
            }
        }
        if let Some(nbf_val) = payload_obj.get("nbf") {
            let nbf = parse_jwt_timestamp_u64(nbf_val).ok_or(DbError::InvalidCredentials)?;
            if now_unix.saturating_add(LOCAL_DEV_JWT_CLOCK_SKEW_SECS) < nbf {
                return Err(DbError::InvalidCredentials);
            }
        }
        if let Some(iat_val) = payload_obj.get("iat") {
            let iat = parse_jwt_timestamp_u64(iat_val).ok_or(DbError::InvalidCredentials)?;
            if iat > now_unix.saturating_add(LOCAL_DEV_JWT_CLOCK_SKEW_SECS) {
                return Err(DbError::InvalidCredentials);
            }
        }
        let jti_opt = match payload_obj.get("jti") {
            Some(v) => Some(
                v.as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or(DbError::InvalidCredentials)?,
            ),
            None => None,
        };
        let sub = payload_obj
            .get("sub")
            .and_then(|v| v.as_str())
            .ok_or(DbError::InvalidCredentials)?;

        let (uid, cid_opt) = {
            let guard = self.read_state();
            let (uid, cid_opt) = if let Some(rest) = sub.strip_prefix("USER:EVE:") {
                (parse_strict_positive_u64(rest)?, None)
            } else if let Some(rest) = sub.strip_prefix("CHARACTER:EVE:") {
                let cid = parse_strict_positive_i64(rest)?;
                let ch = guard
                    .characters
                    .get(&cid)
                    .filter(|c| c.user_id > 0)
                    .ok_or(DbError::InvalidCredentials)?;
                (ch.user_id, Some(cid))
            } else {
                return Err(DbError::InvalidCredentials);
            };

            if let Some(claimed_uid_val) =
                payload_obj.get("user_id").or_else(|| payload_obj.get("uid"))
            {
                if claimed_uid_val.as_u64() != Some(uid) {
                    return Err(DbError::InvalidCredentials);
                }
            }
            if let Some(claimed_cid_val) = payload_obj
                .get("character_id")
                .or_else(|| payload_obj.get("cid"))
            {
                let claimed_cid = claimed_cid_val
                    .as_i64()
                    .filter(|&c| c > 0)
                    .ok_or(DbError::InvalidCredentials)?;
                if cid_opt.is_some_and(|sub_cid| sub_cid != claimed_cid)
                    || !guard
                        .characters
                        .get(&claimed_cid)
                        .is_some_and(|c| c.user_id == uid)
                {
                    return Err(DbError::InvalidCredentials);
                }
            }

            let acc = guard
                .accounts
                .values()
                .find(|a| a.user_id == uid)
                .ok_or(DbError::InvalidCredentials)?;
            acc.check_active_restriction(now_unix)?;
            (uid, cid_opt)
        };

        let rt = self.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
        if let Some(jti) = jti_opt {
            if rt.revoked_jtis.contains(jti) {
                return Err(DbError::InvalidCredentials);
            }
        }
        if rt.kicked_users.contains_key(&uid) {
            return Err(DbError::InvalidCredentials);
        }
        let os = rt
            .online_sessions
            .get(&uid)
            .ok_or(DbError::InvalidCredentials)?;
        let peer_ip_matches = rt.online_session_ips.get(&uid).is_some_and(|&(sid, rec_ip)| {
            sid == os.session_id
                && (rec_ip == norm_peer_ip
                    || (rec_ip == IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
                        && norm_peer_ip == IpAddr::V6(std::net::Ipv6Addr::LOCALHOST))
                    || (rec_ip == IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
                        && norm_peer_ip == IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)))
        });
        if rt.displaced_sessions.contains_key(&os.session_id) || !peer_ip_matches {
            return Err(DbError::InvalidCredentials);
        }

        Ok(QuasarVerifiedToken {
            user_id: uid,
            character_id: cid_opt,
            is_full_sso: false,
        })
    }

    /// Returns a deterministic key identifier (`kid`) for the deployment's SSO JWT signing secret (`ESI-1`).
    pub fn jwt_key_id(&self) -> String {
        let rt = self.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
        let digest = sha256_bytes(&rt.jwt_signing_secret);
        base64url_encode_no_pad(&digest[..12])
    }

    /// Issues an OAuth2 PKCE authorization code (`S256`) for `username` and `character_id` (`D-4`, `ESI-1`).
    /// `character_id == 0` authorizes an account-scoped SSO token (`sub = "USER:EVE:<user_id>"`) for
    /// zero-character launcher accounts (`#472`).
    #[allow(clippy::too_many_arguments)]
    pub fn authorize_pkce(
        &self,
        username: &str,
        password_token: &str,
        character_id: i64,
        client_id: &str,
        redirect_uri: &str,
        scopes: Vec<String>,
        code_challenge: &str,
        code_challenge_method: &str,
        now_unix: u64,
    ) -> Result<String, DbError> {
        if code_challenge_method != "S256" || code_challenge.trim().is_empty() {
            return Err(DbError::ValidationError(
                "PKCE requires S256 code_challenge_method and non-empty code_challenge".into(),
            ));
        }
        if character_id < 0 {
            return Err(DbError::ValidationError("Invalid character_id".into()));
        }
        let acc = self.authenticate_at(username, password_token, now_unix)?;
        if character_id > 0 {
            let _ch = self.select_character_for_user(acc.user_id, character_id)?;
        }
        let code = format!("evomu_pkce_{}", Self::generate_random_salt_hex());
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        rt.pkce_codes.insert(
            code.clone(),
            PkceAuthCode {
                code: code.clone(),
                user_id: acc.user_id,
                character_id,
                client_id: client_id.to_string(),
                redirect_uri: redirect_uri.to_string(),
                scopes,
                code_challenge: code_challenge.to_string(),
                code_challenge_method: code_challenge_method.to_string(),
                expires_at_unix: now_unix.saturating_add(300),
            },
        );
        Ok(code)
    }

    #[allow(clippy::too_many_arguments)]
    fn try_mint_sso_tokens(
        &self,
        user_id: u64,
        character_id: i64,
        subject_name: &str,
        client_id: &str,
        scopes: Vec<String>,
        now_unix: u64,
        revoke_old_token_hash: Option<&str>,
    ) -> Result<SsoTokenBundle, DbError> {
        let (jwt_secret, last_auth_now_unix) = {
            let rt = self.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
            (rt.jwt_signing_secret, rt.last_auth_now_unix.max(now_unix))
        };
        let header_b64 = base64url_encode_no_pad(br#"{"alg":"HS256","typ":"JWT"}"#);
        let jti = Self::generate_random_salt_hex();
        let owner_hash = base64url_encode_no_pad(
            &sha256_bytes(format!("evomu-owner:{user_id}:{character_id}").as_bytes())[..16],
        );
        let sub = if character_id > 0 {
            format!("CHARACTER:EVE:{character_id}")
        } else {
            format!("USER:EVE:{user_id}")
        };
        let claims = SsoTokenClaims {
            sub,
            name: subject_name.to_string(),
            owner: owner_hash,
            user_id,
            character_id,
            scp: scopes.clone(),
            iss: "https://login.eveonline.com".to_string(),
            aud: vec![client_id.to_string(), "EVE Online".to_string()],
            iat: now_unix,
            exp: now_unix.saturating_add(1_200),
            jti: jti.clone(),
        };
        let payload_json = serde_json::to_vec(&claims).expect("serialize SsoTokenClaims");
        let payload_b64 = base64url_encode_no_pad(&payload_json);
        let signing_input = format!("{header_b64}.{payload_b64}");
        let sig = hmac_sha256(&jwt_secret, signing_input.as_bytes());
        let sig_b64 = base64url_encode_no_pad(&sig);
        let access_token = format!("{signing_input}.{sig_b64}");

        let refresh_token = format!("evomu_rt_{}", Self::generate_random_salt_hex());
        let token_hash = sqlite_store::refresh_token_digest(&refresh_token);
        let access_exp = claims.exp;
        let rec = SsoRefreshTokenRecord {
            refresh_token: token_hash.clone(),
            user_id,
            character_id,
            client_id: client_id.to_string(),
            scopes,
            expires_at_unix: now_unix.saturating_add(86_400 * 30),
            revoked: false,
            last_access_token_jti: jti,
            last_access_token_exp_unix: access_exp,
        };

        self.with_auth_sqlite_conn(|conn| {
            sqlite_store::persist_minted_refresh_token(
                conn,
                &token_hash,
                &rec,
                last_auth_now_unix,
            )?;
            if let Some(old_hash) = revoke_old_token_hash {
                if let Err(e) = sqlite_store::persist_refresh_token_revoked(conn, old_hash) {
                    let _ = conn.execute(
                        "DELETE FROM auth_refresh_tokens WHERE token_hash = ?1",
                        rusqlite::params![token_hash],
                    );
                    return Err(e);
                }
            }
            Ok(())
        })
        .map_err(DbError::PersistenceFailure)?;

        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        if let Some(old_hash) = revoke_old_token_hash {
            let Some(old_rec) = rt.refresh_tokens.get_mut(old_hash) else {
                return Err(DbError::InvalidCredentials);
            };
            if old_rec.revoked {
                return Err(DbError::InvalidCredentials);
            }
            old_rec.revoked = true;
        }
        sqlite_store::prune_auth_runtime_locked(&mut rt, now_unix);
        rt.refresh_tokens.insert(token_hash, rec);
        Ok(SsoTokenBundle {
            access_token,
            token_type: "Bearer".to_string(),
            expires_in: 1_200,
            refresh_token,
        })
    }

    /// Exchanges a single-use OAuth2 PKCE authorization code and `code_verifier` for a signed HS256
    /// JWT `access_token` and rotatable `refresh_token` (`D-4`, `ESI-1`).
    pub fn exchange_pkce_code(
        &self,
        code: &str,
        client_id: &str,
        code_verifier: &str,
        now_unix: u64,
    ) -> Result<SsoTokenBundle, DbError> {
        self.exchange_pkce_code_inner(code, client_id, code_verifier, None, false, now_unix)
    }

    /// Exchanges a single-use OAuth2 PKCE authorization code while enforcing `redirect_uri` binding.
    pub fn exchange_pkce_code_with_redirect(
        &self,
        code: &str,
        client_id: &str,
        code_verifier: &str,
        redirect_uri: Option<&str>,
        now_unix: u64,
    ) -> Result<SsoTokenBundle, DbError> {
        self.exchange_pkce_code_inner(code, client_id, code_verifier, redirect_uri, true, now_unix)
    }

    fn exchange_pkce_code_inner(
        &self,
        code: &str,
        client_id: &str,
        code_verifier: &str,
        redirect_uri: Option<&str>,
        enforce_redirect: bool,
        now_unix: u64,
    ) -> Result<SsoTokenBundle, DbError> {
        let pkce = {
            let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
            rt.pkce_codes
                .remove(code)
                .ok_or(DbError::InvalidCredentials)?
        };
        if now_unix > pkce.expires_at_unix || pkce.client_id != client_id || pkce.character_id < 0 {
            return Err(DbError::InvalidCredentials);
        }
        if enforce_redirect
            && !pkce.redirect_uri.is_empty()
            && redirect_uri != Some(pkce.redirect_uri.as_str())
        {
            return Err(DbError::InvalidCredentials);
        }
        let verifier_digest = sha256_bytes(code_verifier.as_bytes());
        let computed_challenge = base64url_encode_no_pad(&verifier_digest);
        if !constant_time_eq(
            computed_challenge.as_bytes(),
            pkce.code_challenge.as_bytes(),
        ) {
            return Err(DbError::InvalidCredentials);
        }
        let acc_username = {
            let guard = self.read_state();
            let acc = guard
                .accounts
                .values()
                .find(|a| a.user_id == pkce.user_id)
                .ok_or(DbError::InvalidCredentials)?;
            acc.check_active_restriction(now_unix)?;
            acc.username.clone()
        };
        let subject_name = if pkce.character_id > 0 {
            self.select_character_for_user(pkce.user_id, pkce.character_id)?
                .name
        } else {
            acc_username
        };
        self.try_mint_sso_tokens(
            pkce.user_id,
            pkce.character_id,
            &subject_name,
            &pkce.client_id,
            pkce.scopes,
            now_unix,
            None,
        )
    }

    /// Rotates an active SSO `refresh_token` and issues a fresh JWT `access_token` + `refresh_token` (`D-4`, `ESI-1`).
    pub fn refresh_sso_token(
        &self,
        refresh_token: &str,
        client_id: &str,
        now_unix: u64,
    ) -> Result<SsoTokenBundle, DbError> {
        let token_hash = sqlite_store::refresh_token_digest(refresh_token);
        let rec = {
            let rt = self.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
            let r = rt
                .refresh_tokens
                .get(&token_hash)
                .ok_or(DbError::InvalidCredentials)?;
            if r.revoked
                || now_unix > r.expires_at_unix
                || r.client_id != client_id
                || r.character_id < 0
            {
                return Err(DbError::InvalidCredentials);
            }
            r.clone()
        };
        let acc_username = {
            let guard = self.read_state();
            let acc = guard
                .accounts
                .values()
                .find(|a| a.user_id == rec.user_id)
                .ok_or(DbError::InvalidCredentials)?;
            acc.check_active_restriction(now_unix)?;
            acc.username.clone()
        };
        let subject_name = if rec.character_id > 0 {
            self.select_character_for_user(rec.user_id, rec.character_id)?
                .name
        } else {
            acc_username
        };
        self.try_mint_sso_tokens(
            rec.user_id,
            rec.character_id,
            &subject_name,
            &rec.client_id,
            rec.scopes,
            now_unix,
            Some(&token_hash),
        )
    }

    /// Verifies a signed HS256 JWT `access_token`, checking header algorithm, signature integrity,
    /// `iat <= now_unix < exp`, kick/token revocation, and account ban/suspension status (`D-4`, `ESI-1`, `R34-P0-1`).
    pub fn verify_sso_access_token(
        &self,
        token: &str,
        now_unix: u64,
    ) -> Result<SsoTokenClaims, DbError> {
        let mut parts = token.splitn(3, '.');
        let (Some(header_b64), Some(payload_b64), Some(sig_b64)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return Err(DbError::InvalidCredentials);
        };
        if sig_b64.is_empty() {
            return Err(DbError::InvalidCredentials);
        }
        let header_bytes =
            base64url_decode_no_pad(header_b64).map_err(|_| DbError::InvalidCredentials)?;
        let header_json: serde_json::Value =
            serde_json::from_slice(&header_bytes).map_err(|_| DbError::InvalidCredentials)?;
        if header_json.get("alg").and_then(|v| v.as_str()) != Some("HS256") {
            return Err(DbError::InvalidCredentials);
        }
        let signing_input = format!("{header_b64}.{payload_b64}");
        let claims: SsoTokenClaims = {
            let rt = self.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
            let expected_sig = hmac_sha256(&rt.jwt_signing_secret, signing_input.as_bytes());
            let expected_sig_b64 = base64url_encode_no_pad(&expected_sig);
            if !constant_time_eq(sig_b64.as_bytes(), expected_sig_b64.as_bytes()) {
                return Err(DbError::InvalidCredentials);
            }
            let payload_bytes =
                base64url_decode_no_pad(payload_b64).map_err(|_| DbError::InvalidCredentials)?;
            let payload_val: serde_json::Value =
                serde_json::from_slice(&payload_bytes).map_err(|_| DbError::InvalidCredentials)?;
            let payload_obj = payload_val
                .as_object()
                .ok_or(DbError::InvalidCredentials)?;
            if payload_contains_forbidden_privilege_claim(payload_obj, true) {
                return Err(DbError::InvalidCredentials);
            }
            let parsed: SsoTokenClaims =
                serde_json::from_slice(&payload_bytes).map_err(|_| DbError::InvalidCredentials)?;
            let expected_sub_ok = if parsed.character_id > 0 {
                parsed.sub == format!("CHARACTER:EVE:{}", parsed.character_id)
            } else if parsed.character_id == 0 && parsed.user_id > 0 {
                parsed.sub == format!("USER:EVE:{}", parsed.user_id)
            } else {
                false
            };
            if parsed.user_id == 0
                || parsed.iat > now_unix
                || now_unix >= parsed.exp
                || parsed.iss != "https://login.eveonline.com"
                || !parsed.aud.iter().any(|a| a == "EVE Online")
                || !expected_sub_ok
                || rt.revoked_jtis.contains(&parsed.jti)
            {
                return Err(DbError::InvalidCredentials);
            }
            if let Some((kicked_at, _)) = rt.kicked_users.get(&parsed.user_id) {
                if *kicked_at >= parsed.iat {
                    return Err(DbError::InvalidCredentials);
                }
            }
            parsed
        };
        let guard = self.read_state();
        let acc = guard
            .accounts
            .values()
            .find(|a| a.user_id == claims.user_id)
            .ok_or(DbError::InvalidCredentials)?;
        acc.check_active_restriction(now_unix)?;
        Ok(claims)
    }

    /// Revokes an SSO refresh token (or access token), failing closed if durable SQLite persistence
    /// fails (`#879`, `#907`).
    pub fn try_revoke_sso_refresh_token(&self, token: &str) -> Result<bool, DbError> {
        let token_hash = sqlite_store::refresh_token_digest(token);
        let (found_refresh, jti_to_revoke, last_auth_now_unix) = {
            let rt = self.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
            if let Some(rec) = rt.refresh_tokens.get(&token_hash) {
                let jti_opt = (!rec.last_access_token_jti.is_empty()).then(|| {
                    (
                        rec.last_access_token_jti.clone(),
                        rec.last_access_token_exp_unix,
                    )
                });
                (true, jti_opt, rt.last_auth_now_unix)
            } else {
                (false, None, rt.last_auth_now_unix)
            }
        };

        if found_refresh {
            self.with_auth_sqlite_conn(|conn| {
                sqlite_store::persist_refresh_token_revoked(conn, &token_hash)?;
                if let Some((ref jti, exp)) = jti_to_revoke {
                    sqlite_store::persist_revoked_jti(conn, jti, exp, last_auth_now_unix)?;
                }
                Ok(())
            })
            .map_err(DbError::PersistenceFailure)?;

            let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
            if let Some(rec) = rt.refresh_tokens.get_mut(&token_hash) {
                rec.revoked = true;
            }
            if let Some((jti, exp)) = jti_to_revoke {
                rt.revoked_jtis.insert(jti.clone());
                if exp > 0 {
                    rt.revoked_jti_expiries.insert(jti, exp);
                }
            }
            return Ok(true);
        }

        if token.matches('.').count() == 2 {
            let mut parts = token.splitn(3, '.');
            if let (Some(_h), Some(payload_b64), Some(_s)) =
                (parts.next(), parts.next(), parts.next())
            {
                if let Ok(payload_bytes) = base64url_decode_no_pad(payload_b64) {
                    if let Ok(parsed) = serde_json::from_slice::<SsoTokenClaims>(&payload_bytes) {
                        self.with_auth_sqlite_conn(|conn| {
                            sqlite_store::persist_revoked_jti(
                                conn,
                                &parsed.jti,
                                parsed.exp,
                                last_auth_now_unix,
                            )
                        })
                        .map_err(DbError::PersistenceFailure)?;

                        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
                        rt.revoked_jtis.insert(parsed.jti.clone());
                        if parsed.exp > 0 {
                            rt.revoked_jti_expiries.insert(parsed.jti, parsed.exp);
                        }
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    /// Revokes an SSO refresh token (or access token) (`D-4`, `ESI-1`).
    pub fn revoke_sso_refresh_token(&self, token: &str) -> bool {
        self.try_revoke_sso_refresh_token(token).unwrap_or(false)
    }
}
