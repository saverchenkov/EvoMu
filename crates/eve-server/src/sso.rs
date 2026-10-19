//! `sso` — Native EVE Online SSO / OAuth2 + PKCE (`S256`) HTTP server (`ESI-1`).
//!
//! Emulates `login.eveonline.com` OAuth2 v2 endpoints for both the V24 launcher/client
//! (`EVE-SSO-CONNECTION`) and third-party ESI applications:
//! - `GET  /v2/oauth/authorize` & `POST /v2/oauth/authorize` (Authorization Code + PKCE `S256`)
//! - `POST /v2/oauth/token` (`grant_type=authorization_code` & `grant_type=refresh_token` rotation)
//! - `POST /v2/oauth/revoke` (Revokes refresh token and paired access token JTI)
//! - `GET  /v2/oauth/verify` & `GET /oauth/verify` (Verifies Bearer JWT signature, expiry, revocation, and scope)
//! - `GET  /oauth/jwks` & `GET /.well-known/oauth-authorization-server`

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use anyhow::Result;
use eve_db::{DbError, SsoTokenClaims};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Maximum HTTP request size accepted by the SSO/ESI listener (256 KiB).
pub const MAX_HTTP_REQUEST_BYTES: usize = 256 * 1024;

/// Structured HTTP response produced by the SSO/ESI HTTP handler.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status_code: u16,
    pub status_text: &'static str,
    pub content_type: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn json(status_code: u16, status_text: &'static str, payload: serde_json::Value) -> Self {
        Self {
            status_code,
            status_text,
            content_type: "application/json; charset=utf-8",
            headers: vec![
                ("Cache-Control".to_string(), "no-store".to_string()),
                ("Pragma".to_string(), "no-cache".to_string()),
            ],
            body: serde_json::to_vec(&payload).unwrap_or_default(),
        }
    }

    pub fn html(status_code: u16, status_text: &'static str, html: String) -> Self {
        Self {
            status_code,
            status_text,
            content_type: "text/html; charset=utf-8",
            headers: vec![("Cache-Control".to_string(), "no-store".to_string())],
            body: html.into_bytes(),
        }
    }

    pub fn to_wire_bytes(&self) -> Vec<u8> {
        let mut head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.status_code,
            self.status_text,
            self.content_type,
            self.body.len()
        );
        for (k, v) in &self.headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("\r\n");
        let mut out = head.into_bytes();
        out.extend_from_slice(&self.body);
        out
    }
}

/// Handles a single HTTP/1.1 connection on the SSO/ESI listener (`ESI-1`).
pub async fn handle_http_connection(mut stream: TcpStream, ctx: Arc<ServerContext>) -> Result<()> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > MAX_HTTP_REQUEST_BYTES {
            let resp = HttpResponse::json(
                413,
                "Payload Too Large",
                json!({"error": "invalid_request", "error_description": "Request headers too large"}),
            );
            stream.write_all(&resp.to_wire_bytes()).await?;
            return Ok(());
        }
    };

    let header_str = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = header_str.split("\r\n");
    let Some(request_line) = lines.next() else {
        return Ok(());
    };
    let mut req_parts = request_line.split_whitespace();
    let method = req_parts.next().unwrap_or("GET").to_uppercase();
    let target = req_parts.next().unwrap_or("/").to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    let content_length = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    if content_length > MAX_HTTP_REQUEST_BYTES {
        let resp = HttpResponse::json(
            413,
            "Payload Too Large",
            json!({"error": "invalid_request", "error_description": "Request body too large"}),
        );
        stream.write_all(&resp.to_wire_bytes()).await?;
        return Ok(());
    }

    while buf.len() < header_end + content_length {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }

    let body_end = (header_end + content_length).min(buf.len());
    let body = &buf[header_end..body_end];

    let resp = handle_http_request(&ctx, &method, &target, &headers, body);
    stream.write_all(&resp.to_wire_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

/// Verifies an `Authorization: Bearer <jwt>` header against `WorldRepository::verify_sso_access_token`
/// and optional route scope requirements (`ESI-1`, `feat:esi.core.sso-tokens`).
/// Returns `401 Unauthorized` on missing/invalid/expired/revoked tokens, and `403 Forbidden` on
/// insufficient scope or banned/suspended accounts.
pub fn verify_esi_bearer_token(
    ctx: &ServerContext,
    headers: &HashMap<String, String>,
    required_scope: Option<&str>,
) -> std::result::Result<SsoTokenClaims, HttpResponse> {
    let Some(auth_hdr) = headers.get("authorization") else {
        return Err(HttpResponse::json(
            401,
            "Unauthorized",
            json!({
                "error": "invalid_token",
                "error_description": "Missing Authorization: Bearer <token> header"
            }),
        ));
    };
    let token = auth_hdr
        .strip_prefix("Bearer ")
        .or_else(|| auth_hdr.strip_prefix("bearer "))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            HttpResponse::json(
                401,
                "Unauthorized",
                json!({
                    "error": "invalid_token",
                    "error_description": "Authorization header must use Bearer scheme"
                }),
            )
        })?;

    let now_unix = ctx.now_unix();
    // First verify JWT signature, expiry, revocation, and account status without scope check
    // so we can distinguish 401 (invalid/expired/revoked token) from 403 (insufficient scope or banned).
    let claims = match ctx.db.verify_sso_access_token(token, now_unix) {
        Ok(c) => c,
        Err(DbError::AccountBanned { until_unix, reason }) => {
            return Err(HttpResponse::json(
                403,
                "Forbidden",
                json!({
                    "error": "account_banned",
                    "error_description": format!("Account banned until unix={until_unix}: {reason}"),
                    "banned_until_unix": until_unix,
                    "reason": reason
                }),
            ));
        }
        Err(DbError::AccountSuspended { until_unix, reason }) => {
            return Err(HttpResponse::json(
                403,
                "Forbidden",
                json!({
                    "error": "account_suspended",
                    "error_description": format!("Account suspended until unix={until_unix}: {reason}"),
                    "suspended_until_unix": until_unix,
                    "reason": reason
                }),
            ));
        }
        Err(e) => {
            return Err(HttpResponse::json(
                401,
                "Unauthorized",
                json!({
                    "error": "invalid_token",
                    "error_description": e.to_string()
                }),
            ));
        }
    };

    if let Some(req_scope) = required_scope.filter(|s| !s.is_empty()) {
        if !claims.scp.iter().any(|s| s == req_scope) {
            return Err(HttpResponse::json(
                403,
                "Forbidden",
                json!({
                    "error": "insufficient_scope",
                    "error_description": format!("SSO token missing required ESI scope '{req_scope}'"),
                    "required_scope": req_scope
                }),
            ));
        }
    }

    Ok(claims)
}

/// Dispatches an HTTP request to the SSO / OAuth2 + PKCE (`S256`) or ESI verification handlers (`ESI-1`).
pub fn handle_http_request(
    ctx: &ServerContext,
    method: &str,
    target: &str,
    headers: &HashMap<String, String>,
    body: &[u8],
) -> HttpResponse {
    let (path, query_str) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };
    let query_params = parse_urlencoded(query_str);
    let body_params = parse_request_body(headers, body);

    match (method, path) {
        ("GET", "/.well-known/oauth-authorization-server")
        | ("GET", "/.well-known/openid-configuration") => {
            let base = headers
                .get("host")
                .map(|h| format!("http://{h}"))
                .unwrap_or_else(|| format!("http://127.0.0.1:{}", ctx.esi_port));
            HttpResponse::json(
                200,
                "OK",
                json!({
                    "issuer": "https://login.eveonline.com",
                    "authorization_endpoint": format!("{base}/v2/oauth/authorize"),
                    "token_endpoint": format!("{base}/v2/oauth/token"),
                    "revocation_endpoint": format!("{base}/v2/oauth/revoke"),
                    "jwks_uri": format!("{base}/oauth/jwks"),
                    "response_types_supported": ["code"],
                    "grant_types_supported": ["authorization_code", "refresh_token"],
                    "code_challenge_methods_supported": ["S256"],
                    "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post", "none"]
                }),
            )
        }

        ("GET", "/oauth/jwks") | ("GET", "/.well-known/jwks.json") => HttpResponse::json(
            200,
            "OK",
            json!({
                "keys": [
                    {
                        "kty": "oct",
                        "alg": "HS256",
                        "use": "sig",
                        "kid": ctx.db.jwt_key_id()
                    }
                ]
            }),
        ),

        ("GET", "/v2/oauth/authorize") | ("POST", "/v2/oauth/authorize") => {
            handle_oauth_authorize(ctx, method, headers, &query_params, &body_params)
        }

        ("POST", "/v2/oauth/token") | ("POST", "/oauth/token") => {
            handle_oauth_token(ctx, headers, &query_params, &body_params)
        }

        ("POST", "/v2/oauth/revoke") | ("POST", "/oauth/revoke") => {
            handle_oauth_revoke(ctx, headers, &query_params, &body_params)
        }

        ("GET", "/v2/oauth/verify") | ("GET", "/oauth/verify") => {
            let req_scope = query_params
                .get("scope")
                .or_else(|| query_params.get("required_scope"))
                .or_else(|| headers.get("x-required-scope"))
                .map(String::as_str);
            match verify_esi_bearer_token(ctx, headers, req_scope) {
                Ok(claims) => HttpResponse::json(
                    200,
                    "OK",
                    json!({
                        "CharacterID": claims.character_id,
                        "CharacterName": claims.name,
                        "ExpiresOn": format_unix_iso8601(claims.exp),
                        "Scopes": claims.scp.join(" "),
                        "TokenType": "Character",
                        "CharacterOwnerHash": claims.owner,
                        "IntellectualProperty": "EVE",
                        "UserID": claims.user_id
                    }),
                ),
                Err(err_resp) => err_resp,
            }
        }

        _ => HttpResponse::json(
            404,
            "Not Found",
            json!({
                "error": "not_found",
                "error_description": format!("Unknown SSO/ESI endpoint: {method} {path}")
            }),
        ),
    }
}

fn handle_oauth_authorize(
    ctx: &ServerContext,
    method: &str,
    headers: &HashMap<String, String>,
    query: &HashMap<String, String>,
    body: &HashMap<String, String>,
) -> HttpResponse {
    let get_param = |key: &str| -> Option<String> {
        body.get(key)
            .or_else(|| query.get(key))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };

    let response_type = get_param("response_type").unwrap_or_else(|| "code".to_string());
    if response_type != "code" {
        return HttpResponse::json(
            400,
            "Bad Request",
            json!({
                "error": "unsupported_response_type",
                "error_description": "response_type must be 'code'"
            }),
        );
    }

    let Some(client_id) = get_param("client_id") else {
        return HttpResponse::json(
            400,
            "Bad Request",
            json!({
                "error": "invalid_request",
                "error_description": "Missing required parameter: client_id"
            }),
        );
    };
    let Some(redirect_uri) = get_param("redirect_uri") else {
        return HttpResponse::json(
            400,
            "Bad Request",
            json!({
                "error": "invalid_request",
                "error_description": "Missing required parameter: redirect_uri"
            }),
        );
    };
    let Some(code_challenge) = get_param("code_challenge") else {
        return HttpResponse::json(
            400,
            "Bad Request",
            json!({
                "error": "invalid_request",
                "error_description": "Missing required PKCE parameter: code_challenge"
            }),
        );
    };
    let code_challenge_method =
        get_param("code_challenge_method").unwrap_or_else(|| "S256".to_string());
    if code_challenge_method != "S256" {
        return HttpResponse::json(
            400,
            "Bad Request",
            json!({
                "error": "invalid_request",
                "error_description": "PKCE code_challenge_method must be 'S256'"
            }),
        );
    }

    let scope_raw = get_param("scope").unwrap_or_default();
    let scopes: Vec<String> = scope_raw
        .split(|c: char| c.is_whitespace() || c == ',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let state = get_param("state");

    // Extract credentials from form/query/JSON or `Authorization: Basic ...`
    let basic_creds = headers
        .get("authorization")
        .and_then(|h| parse_basic_auth(h));
    let username = get_param("username").or_else(|| basic_creds.as_ref().map(|(u, _)| u.clone()));
    let password = get_param("password").or_else(|| basic_creds.as_ref().map(|(_, p)| p.clone()));
    let explicit_character_id = get_param("character_id").and_then(|s| s.parse::<i64>().ok());

    // If GET `/v2/oauth/authorize` is invoked by a browser/launcher without credentials yet,
    // render the interactive HTML login/consent form.
    if method == "GET" && username.is_none() && password.is_none() {
        let state_val = state.clone().unwrap_or_default();
        let html = format!(
            "<!DOCTYPE html><html><head><title>EVE Online SSO</title></head><body>\
            <form method=\"POST\" action=\"/v2/oauth/authorize\">\
            <input type=\"hidden\" name=\"response_type\" value=\"code\"/>\
            <input type=\"hidden\" name=\"client_id\" value=\"{client_id}\"/>\
            <input type=\"hidden\" name=\"redirect_uri\" value=\"{redirect_uri}\"/>\
            <input type=\"hidden\" name=\"code_challenge\" value=\"{code_challenge}\"/>\
            <input type=\"hidden\" name=\"code_challenge_method\" value=\"{code_challenge_method}\"/>\
            <input type=\"hidden\" name=\"scope\" value=\"{scope_raw}\"/>\
            <input type=\"hidden\" name=\"state\" value=\"{state_val}\"/>\
            <input type=\"text\" name=\"username\" placeholder=\"Username\"/>\
            <input type=\"password\" name=\"password\" placeholder=\"Password\"/>\
            <button type=\"submit\">Authorize</button>\
            </form></body></html>"
        );
        return HttpResponse::html(200, "OK", html);
    }

    let Some(user) = username else {
        return HttpResponse::json(
            401,
            "Unauthorized",
            json!({
                "error": "access_denied",
                "error_description": "Missing username for SSO authorization"
            }),
        );
    };
    let Some(pass) = password else {
        return HttpResponse::json(
            401,
            "Unauthorized",
            json!({
                "error": "access_denied",
                "error_description": "Missing password for SSO authorization"
            }),
        );
    };

    let now_unix = ctx.now_unix();
    let acc = match ctx.db.authenticate_at(&user, &pass, now_unix) {
        Ok(a) => a,
        Err(DbError::AccountBanned { until_unix, reason }) => {
            return HttpResponse::json(
                403,
                "Forbidden",
                json!({
                    "error": "access_denied",
                    "error_description": format!("Account banned until unix={until_unix}: {reason}"),
                    "banned_until_unix": until_unix,
                    "reason": reason
                }),
            );
        }
        Err(DbError::AccountSuspended { until_unix, reason }) => {
            return HttpResponse::json(
                403,
                "Forbidden",
                json!({
                    "error": "access_denied",
                    "error_description": format!("Account suspended until unix={until_unix}: {reason}"),
                    "suspended_until_unix": until_unix,
                    "reason": reason
                }),
            );
        }
        Err(_) => {
            return HttpResponse::json(
                401,
                "Unauthorized",
                json!({
                    "error": "access_denied",
                    "error_description": "Invalid username or password"
                }),
            );
        }
    };

    let character_id = match explicit_character_id {
        Some(cid) => cid,
        None => {
            let chars = ctx.db.get_characters_for_user(acc.user_id);
            let Some(first) = chars.first() else {
                return HttpResponse::json(
                    400,
                    "Bad Request",
                    json!({
                        "error": "invalid_request",
                        "error_description": "Account has no characters to authorize"
                    }),
                );
            };
            first.character_id
        }
    };

    match ctx.db.authorize_pkce(
        &user,
        &pass,
        character_id,
        &client_id,
        &redirect_uri,
        scopes,
        &code_challenge,
        &code_challenge_method,
        now_unix,
    ) {
        Ok(code) => {
            let sep = if redirect_uri.contains('?') { '&' } else { '?' };
            let location = match &state {
                Some(st) => format!("{redirect_uri}{sep}code={code}&state={st}"),
                None => format!("{redirect_uri}{sep}code={code}"),
            };
            let mut resp = HttpResponse::json(
                302,
                "Found",
                json!({
                    "code": code,
                    "state": state,
                    "redirect_uri": location
                }),
            );
            resp.headers.push(("Location".to_string(), location));
            resp
        }
        Err(DbError::AccountBanned { until_unix, reason }) => HttpResponse::json(
            403,
            "Forbidden",
            json!({
                "error": "access_denied",
                "error_description": format!("Account banned until unix={until_unix}: {reason}"),
                "banned_until_unix": until_unix,
                "reason": reason
            }),
        ),
        Err(DbError::AccountSuspended { until_unix, reason }) => HttpResponse::json(
            403,
            "Forbidden",
            json!({
                "error": "access_denied",
                "error_description": format!("Account suspended until unix={until_unix}: {reason}"),
                "suspended_until_unix": until_unix,
                "reason": reason
            }),
        ),
        Err(DbError::InvalidCredentials) => HttpResponse::json(
            401,
            "Unauthorized",
            json!({
                "error": "access_denied",
                "error_description": "Invalid username or password"
            }),
        ),
        Err(e) => HttpResponse::json(
            400,
            "Bad Request",
            json!({
                "error": "invalid_request",
                "error_description": e.to_string()
            }),
        ),
    }
}

fn handle_oauth_token(
    ctx: &ServerContext,
    headers: &HashMap<String, String>,
    query: &HashMap<String, String>,
    body: &HashMap<String, String>,
) -> HttpResponse {
    let get_param = |key: &str| -> Option<String> {
        body.get(key)
            .or_else(|| query.get(key))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };

    let basic_client_id = headers
        .get("authorization")
        .and_then(|h| parse_basic_auth(h))
        .map(|(cid, _)| cid);
    let client_id = get_param("client_id").or(basic_client_id).unwrap_or_default();
    if client_id.is_empty() {
        return HttpResponse::json(
            400,
            "Bad Request",
            json!({
                "error": "invalid_client",
                "error_description": "Missing client_id in request body or Basic Authorization header"
            }),
        );
    }

    let Some(grant_type) = get_param("grant_type") else {
        return HttpResponse::json(
            400,
            "Bad Request",
            json!({
                "error": "invalid_request",
                "error_description": "Missing required parameter: grant_type"
            }),
        );
    };

    let now_unix = ctx.now_unix();
    match grant_type.as_str() {
        "authorization_code" => {
            let Some(code) = get_param("code") else {
                return HttpResponse::json(
                    400,
                    "Bad Request",
                    json!({
                        "error": "invalid_request",
                        "error_description": "Missing required parameter: code"
                    }),
                );
            };
            let Some(code_verifier) = get_param("code_verifier") else {
                return HttpResponse::json(
                    400,
                    "Bad Request",
                    json!({
                        "error": "invalid_request",
                        "error_description": "Missing required PKCE parameter: code_verifier"
                    }),
                );
            };
            match ctx
                .db
                .exchange_pkce_code(&code, &client_id, &code_verifier, now_unix)
            {
                Ok(tok) => HttpResponse::json(
                    200,
                    "OK",
                    json!({
                        "access_token": tok.access_token,
                        "token_type": tok.token_type,
                        "expires_in": tok.expires_in,
                        "refresh_token": tok.refresh_token
                    }),
                ),
                Err(DbError::AccountBanned { until_unix, reason }) => HttpResponse::json(
                    403,
                    "Forbidden",
                    json!({
                        "error": "access_denied",
                        "error_description": format!("Account banned until unix={until_unix}: {reason}")
                    }),
                ),
                Err(DbError::AccountSuspended { until_unix, reason }) => HttpResponse::json(
                    403,
                    "Forbidden",
                    json!({
                        "error": "access_denied",
                        "error_description": format!("Account suspended until unix={until_unix}: {reason}")
                    }),
                ),
                Err(e) => HttpResponse::json(
                    400,
                    "Bad Request",
                    json!({
                        "error": "invalid_grant",
                        "error_description": e.to_string()
                    }),
                ),
            }
        }
        "refresh_token" => {
            let Some(refresh_token) = get_param("refresh_token") else {
                return HttpResponse::json(
                    400,
                    "Bad Request",
                    json!({
                        "error": "invalid_request",
                        "error_description": "Missing required parameter: refresh_token"
                    }),
                );
            };
            match ctx
                .db
                .refresh_sso_token(&refresh_token, &client_id, now_unix)
            {
                Ok(tok) => HttpResponse::json(
                    200,
                    "OK",
                    json!({
                        "access_token": tok.access_token,
                        "token_type": tok.token_type,
                        "expires_in": tok.expires_in,
                        "refresh_token": tok.refresh_token
                    }),
                ),
                Err(DbError::AccountBanned { until_unix, reason }) => HttpResponse::json(
                    403,
                    "Forbidden",
                    json!({
                        "error": "access_denied",
                        "error_description": format!("Account banned until unix={until_unix}: {reason}")
                    }),
                ),
                Err(DbError::AccountSuspended { until_unix, reason }) => HttpResponse::json(
                    403,
                    "Forbidden",
                    json!({
                        "error": "access_denied",
                        "error_description": format!("Account suspended until unix={until_unix}: {reason}")
                    }),
                ),
                Err(e) => HttpResponse::json(
                    400,
                    "Bad Request",
                    json!({
                        "error": "invalid_grant",
                        "error_description": e.to_string()
                    }),
                ),
            }
        }
        _ => HttpResponse::json(
            400,
            "Bad Request",
            json!({
                "error": "unsupported_grant_type",
                "error_description": format!("Unsupported grant_type: {grant_type}")
            }),
        ),
    }
}

fn handle_oauth_revoke(
    ctx: &ServerContext,
    _headers: &HashMap<String, String>,
    query: &HashMap<String, String>,
    body: &HashMap<String, String>,
) -> HttpResponse {
    let get_param = |key: &str| -> Option<String> {
        body.get(key)
            .or_else(|| query.get(key))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };

    let Some(token) = get_param("token") else {
        return HttpResponse::json(
            400,
            "Bad Request",
            json!({
                "error": "invalid_request",
                "error_description": "Missing required parameter: token"
            }),
        );
    };

    let revoked = ctx.db.revoke_sso_refresh_token(&token);
    HttpResponse::json(200, "OK", json!({ "revoked": revoked }))
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn parse_request_body(
    headers: &HashMap<String, String>,
    body: &[u8],
) -> HashMap<String, String> {
    if body.is_empty() {
        return HashMap::new();
    }
    let content_type = headers
        .get("content-type")
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if content_type.contains("application/json") || body.first() == Some(&b'{') {
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_slice::<serde_json::Value>(body)
        {
            let mut out = HashMap::new();
            for (k, v) in map {
                match v {
                    serde_json::Value::String(s) => {
                        out.insert(k, s);
                    }
                    serde_json::Value::Number(n) => {
                        out.insert(k, n.to_string());
                    }
                    serde_json::Value::Bool(b) => {
                        out.insert(k, b.to_string());
                    }
                    serde_json::Value::Array(arr) => {
                        let joined: Vec<String> = arr
                            .into_iter()
                            .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                            .collect();
                        out.insert(k, joined.join(" "));
                    }
                    _ => {}
                }
            }
            return out;
        }
    }
    parse_urlencoded(&String::from_utf8_lossy(body))
}

fn parse_urlencoded(input: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in input.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        map.insert(percent_decode(k), percent_decode(v));
    }
    map
}

fn percent_decode(input: &str) -> String {
    let mut bytes = Vec::with_capacity(input.len());
    let raw = input.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        match raw[i] {
            b'+' => {
                bytes.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < raw.len() => {
                if let (Some(hi), Some(lo)) = (hex_val(raw[i + 1]), hex_val(raw[i + 2])) {
                    bytes.push((hi << 4) | lo);
                    i += 3;
                } else {
                    bytes.push(raw[i]);
                    i += 1;
                }
            }
            b => {
                bytes.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn parse_basic_auth(header_val: &str) -> Option<(String, String)> {
    let encoded = header_val
        .strip_prefix("Basic ")
        .or_else(|| header_val.strip_prefix("basic "))?
        .trim();
    let decoded = base64_decode_standard(encoded)?;
    let s = String::from_utf8(decoded).ok()?;
    let (u, p) = s.split_once(':')?;
    Some((u.to_string(), p.to_string()))
}

fn base64_decode_standard(input: &str) -> Option<Vec<u8>> {
    let mut buf = 0u32;
    let mut bits = 0u8;
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    for b in input.bytes() {
        if b == b'=' || b.is_ascii_whitespace() {
            continue;
        }
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        buf = (buf << 6) | (val as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    Some(out)
}

fn format_unix_iso8601(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let rem = unix_secs % 86_400;
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let second = rem % 60;

    // Civil date from days since 1970-01-01 (Howard Hinnant algorithm)
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!("{year:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}")
}
