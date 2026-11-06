//! Quasar gRPC gateway JWT issuance and strict cryptographic verification (`R6-T4-01`).
//!
//! Eliminates all hardcoded `"dev"` JWT secret acceptance while supporting both:
//! - Deployment OAuth2/PKCE SSO HS256 JWTs (`verify_sso_access_token`, full role set), and
//! - Local-dev gateway HS256 JWTs signed strictly against the server's own secret
//!   (`AuthRuntime::jwt_signing_secret`) with mandatory `exp`, strict `sub` ownership validation,
//!   zero injected privilege claims, and suppressed `ROLE_GM_MASK` elevation.

#![forbid(unsafe_code)]

use crate::accounts::{
    base64url_decode_no_pad, base64url_encode_no_pad, constant_time_eq, hmac_sha256,
};
use crate::persistence::WorldRepository;
use crate::types::DbError;
use serde_json::{Map, Value};
use std::sync::atomic::{AtomicU64, Ordering};

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

impl WorldRepository {
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
            let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
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
                let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
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

            let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
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

            if let Some((kicked_at, _)) = rt.kicked_users.get(&uid) {
                if iat_opt.is_none_or(|iat| *kicked_at >= iat) {
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

        Ok(QuasarVerifiedToken {
            user_id: resolved_user_id,
            character_id: token_char_id,
            is_full_sso: false,
        })
    }
}
