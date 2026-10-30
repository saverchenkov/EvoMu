//! `esi::router` — OpenAPI-conformant ESI HTTP router, OAuth2 Bearer authenticator, scope/character
//! guard, error-limit window tracker, and conditional `ETag` cache layer (`ESI-2`).

use crate::esi::openapi::OpenApiSpec;
use crate::esi::sample_routes::{self, format_iso8601_utc, RouteExecutionResult};
use crate::esi::{
    compute_etag, format_http_date, if_none_match_matches, EsiHttpRequest, EsiHttpResponse,
};
use crate::router::ServerContext;
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

const MAX_ERROR_LIMIT_CLIENTS: usize = 4096;

#[derive(Debug, Clone)]
struct ErrorLimitWindow {
    window_start_unix: u64,
    errors_used: u32,
}

pub struct EsiRouter {
    pub ctx: Arc<ServerContext>,
    pub started_at_unix: u64,
    pub error_limit_max: u32,
    pub error_limit_window_secs: u64,
    error_windows: RwLock<HashMap<String, ErrorLimitWindow>>,
    request_seq: AtomicU64,
}

impl EsiRouter {
    /// Creates a new `EsiRouter` with the standard ESI error limit (`100` errors per `60`s window).
    pub fn new(ctx: Arc<ServerContext>) -> Self {
        Self::with_error_limit(ctx, 100, 60)
    }

    /// Creates a new `EsiRouter` with a custom error-limit budget and window duration.
    pub fn with_error_limit(
        ctx: Arc<ServerContext>,
        error_limit_max: u32,
        error_limit_window_secs: u64,
    ) -> Self {
        let now = ctx.now_unix();
        Self {
            ctx,
            started_at_unix: now,
            error_limit_max: error_limit_max.max(1),
            error_limit_window_secs: error_limit_window_secs.max(1),
            error_windows: RwLock::new(HashMap::new()),
            request_seq: AtomicU64::new(1),
        }
    }

    fn client_bucket_key(req: &EsiHttpRequest) -> String {
        let ip = req.peer_ip.trim();
        if ip.is_empty() {
            "127.0.0.1".to_string()
        } else {
            ip.to_string()
        }
    }

    fn prune_error_windows_if_full(
        map: &mut HashMap<String, ErrorLimitWindow>,
        now: u64,
        window_secs: u64,
    ) {
        if map.len() < MAX_ERROR_LIMIT_CLIENTS {
            return;
        }
        map.retain(|_, w| now < w.window_start_unix.saturating_add(window_secs));
        if map.len() >= MAX_ERROR_LIMIT_CLIENTS {
            if let Some(oldest_key) = map
                .iter()
                .min_by_key(|(_, w)| w.window_start_unix)
                .map(|(k, _)| k.clone())
            {
                map.remove(&oldest_key);
            }
        }
    }

    fn current_error_window_snapshot(&self, client_key: &str, now: u64) -> (u32, u64, bool) {
        let mut map = self.error_windows.write().unwrap_or_else(|e| e.into_inner());
        if !map.contains_key(client_key) {
            Self::prune_error_windows_if_full(&mut map, now, self.error_limit_window_secs);
        }
        let win = map.entry(client_key.to_string()).or_insert(ErrorLimitWindow {
            window_start_unix: now,
            errors_used: 0,
        });
        if win.window_start_unix == 0
            || now >= win.window_start_unix.saturating_add(self.error_limit_window_secs)
        {
            win.window_start_unix = now;
            win.errors_used = 0;
        }
        let reset = win
            .window_start_unix
            .saturating_add(self.error_limit_window_secs)
            .saturating_sub(now)
            .max(1);
        let exhausted = win.errors_used >= self.error_limit_max;
        let remain = self.error_limit_max.saturating_sub(win.errors_used);
        (remain, reset, exhausted)
    }

    fn record_error_and_snapshot(&self, client_key: &str, now: u64) -> (u32, u64) {
        let mut map = self.error_windows.write().unwrap_or_else(|e| e.into_inner());
        if !map.contains_key(client_key) {
            Self::prune_error_windows_if_full(&mut map, now, self.error_limit_window_secs);
        }
        let win = map.entry(client_key.to_string()).or_insert(ErrorLimitWindow {
            window_start_unix: now,
            errors_used: 0,
        });
        if win.window_start_unix == 0
            || now >= win.window_start_unix.saturating_add(self.error_limit_window_secs)
        {
            win.window_start_unix = now;
            win.errors_used = 0;
        }
        win.errors_used = win.errors_used.saturating_add(1);
        let remain = self.error_limit_max.saturating_sub(win.errors_used);
        let reset = win
            .window_start_unix
            .saturating_add(self.error_limit_window_secs)
            .saturating_sub(now)
            .max(1);
        (remain, reset)
    }

    fn attach_esi_base_headers(
        headers: &mut HashMap<String, String>,
        remain: u32,
        reset: u64,
        req_id: u64,
        compat_date: &str,
    ) {
        headers.insert(
            "X-Esi-Error-Limit-Remain".to_string(),
            remain.to_string(),
        );
        headers.insert("X-Esi-Error-Limit-Reset".to_string(), reset.to_string());
        headers.insert(
            "X-Esi-Request-Id".to_string(),
            format!("evomu-esi-{req_id:08x}"),
        );
        headers.insert(
            "X-Compatibility-Date".to_string(),
            compat_date.to_string(),
        );
    }

    fn build_error_response(
        &self,
        client_key: &str,
        now: u64,
        req_id: u64,
        compat_date: &str,
        status_code: u16,
        message: impl Into<String>,
        count_against_error_limit: bool,
    ) -> EsiHttpResponse {
        let (remain, reset) = if count_against_error_limit {
            self.record_error_and_snapshot(client_key, now)
        } else {
            let (rem, res, _) = self.current_error_window_snapshot(client_key, now);
            (rem, res)
        };
        let mut headers = HashMap::new();
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        Self::attach_esi_base_headers(&mut headers, remain, reset, req_id, compat_date);
        let payload = json!({
            "error": message.into(),
            "status": status_code,
        });
        let body = serde_json::to_vec(&payload).unwrap_or_default();
        EsiHttpResponse {
            status_code,
            headers,
            body,
        }
    }

    /// Processes an incoming `EsiHttpRequest` and returns an OpenAPI-conformant `EsiHttpResponse`.
    pub fn handle_request(&self, req: &EsiHttpRequest) -> EsiHttpResponse {
        let now = self.ctx.now_unix();
        let req_id = self.request_seq.fetch_add(1, Ordering::Relaxed);
        let client_key = Self::client_bucket_key(req);
        let (raw_path, query_params) = split_path_and_query(&req.raw_target);

        let compat_date = req
            .header("x-compatibility-date")
            .or_else(|| query_params.get("compatibility_date").map(|s| s.as_str()))
            .unwrap_or("2020-01-01");

        // Enforce per-client error-limit window (HTTP 420 Error Limited when remain == 0).
        let (remain_initial, reset_initial, exhausted) =
            self.current_error_window_snapshot(&client_key, now);
        if exhausted {
            let mut headers = HashMap::new();
            headers.insert("Content-Type".to_string(), "application/json".to_string());
            Self::attach_esi_base_headers(
                &mut headers,
                0,
                reset_initial,
                req_id,
                compat_date,
            );
            let body = serde_json::to_vec(&json!({
                "error": "This software has exceeded the error limit for ESI.",
                "status": 420,
            }))
            .unwrap_or_default();
            return EsiHttpResponse {
                status_code: 420,
                headers,
                body,
            };
        }

        let spec = OpenApiSpec::pinned();

        // Validate compatibility date against the pinned OpenAPI spec enum.
        if !spec.compatibility_dates.iter().any(|d| d == compat_date) {
            return self.build_error_response(
                &client_key,
                now,
                req_id,
                "2020-01-01",
                400,
                format!("Unsupported X-Compatibility-Date '{compat_date}'"),
                true,
            );
        }

        // Validate datasource / X-Tenant parameter.
        if let Some(ds) = query_params
            .get("datasource")
            .map(|s| s.as_str())
            .or_else(|| req.header("x-tenant"))
        {
            if !ds.is_empty() && !ds.eq_ignore_ascii_case("tranquility") {
                return self.build_error_response(
                    &client_key,
                    now,
                    req_id,
                    compat_date,
                    400,
                    format!("Invalid datasource '{ds}' (expected 'tranquility')"),
                    true,
                );
            }
        }

        let (_, canonical_path) = OpenApiSpec::strip_version_prefix(raw_path);

        // Handle ESI Meta / Introspection endpoints (`feat:esi.core.meta`):
        // /swagger.json, /openapi.json, /versions/, /status.json, /verify/
        if req.method.eq_ignore_ascii_case("GET") {
            match canonical_path.as_str() {
                "/swagger.json" | "/openapi.json" => {
                    let body = serde_json::to_vec(&spec.raw_root).unwrap_or_default();
                    return self.finalize_cached_response(
                        req,
                        now,
                        req_id,
                        compat_date,
                        remain_initial,
                        reset_initial,
                        200,
                        body,
                        3600,
                        false,
                        false,
                        None,
                    );
                }
                "/meta" => {
                    let body = serde_json::to_vec(&json!({
                        "compatibility_dates": "/meta/compatibility-dates",
                        "changelog": "/meta/changelog",
                        "swagger": "/swagger.json",
                        "openapi": "/openapi.json",
                        "versions": "/versions",
                        "status": "/status.json",
                    }))
                    .unwrap_or_default();
                    return self.finalize_cached_response(
                        req,
                        now,
                        req_id,
                        compat_date,
                        remain_initial,
                        reset_initial,
                        200,
                        body,
                        3600,
                        false,
                        false,
                        None,
                    );
                }
                "/versions" => {
                    let body = serde_json::to_vec(&json!([
                        "latest", "v1", "v2", "v3", "v4", "v5", "v6", "dev", "legacy"
                    ]))
                    .unwrap_or_default();
                    return self.finalize_cached_response(
                        req,
                        now,
                        req_id,
                        compat_date,
                        remain_initial,
                        reset_initial,
                        200,
                        body,
                        3600,
                        false,
                        false,
                        None,
                    );
                }
                "/status.json" => {
                    let status_entries: Vec<serde_json::Value> = spec
                        .operations
                        .iter()
                        .map(|op| {
                            let status = if sample_routes::is_operation_implemented(
                                &op.method,
                                &op.path_pattern,
                            ) {
                                "green"
                            } else {
                                "red"
                            };
                            json!({
                                "endpoint": op.operation_id,
                                "method": op.method.to_ascii_lowercase(),
                                "route": op.path_pattern,
                                "status": status,
                                "tags": op.tags,
                            })
                        })
                        .collect();
                    let body = serde_json::to_vec(&status_entries).unwrap_or_default();
                    return self.finalize_cached_response(
                        req,
                        now,
                        req_id,
                        compat_date,
                        remain_initial,
                        reset_initial,
                        200,
                        body,
                        60,
                        false,
                        false,
                        None,
                    );
                }
                "/verify" | "/oauth/verify" => {
                    let Some(raw_token) = extract_bearer_token(req, &query_params) else {
                        return self.build_error_response(
                            &client_key,
                            now,
                            req_id,
                            compat_date,
                            401,
                            "Missing Authorization Bearer token for /verify/",
                            true,
                        );
                    };
                    let claims = match self.ctx.db.verify_sso_access_token(&raw_token, now) {
                        Ok(c) => c,
                        Err(e) => {
                            return self.build_error_response(
                                &client_key,
                                now,
                                req_id,
                                compat_date,
                                401,
                                format!("Invalid or expired SSO token: {e}"),
                                true,
                            );
                        }
                    };
                    let verify_body = serde_json::to_vec(&json!({
                        "CharacterID": claims.character_id,
                        "CharacterName": claims.name,
                        "ExpiresOn": format_iso8601_utc(claims.exp),
                        "Scopes": claims.scp.join(" "),
                        "TokenType": "Character",
                        "CharacterOwnerHash": claims.owner,
                        "IntellectualProperty": "EVE",
                    }))
                    .unwrap_or_default();
                    return self.finalize_cached_response(
                        req,
                        now,
                        req_id,
                        compat_date,
                        remain_initial,
                        reset_initial,
                        200,
                        verify_body,
                        0,
                        true,
                        false,
                        None,
                    );
                }
                p if p.starts_with("/images/characters/") && p.ends_with("/portrait") => {
                    let parts: Vec<&str> = p.split('/').collect();
                    let char_id = parts.get(3).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
                    let size = query_params.get("size").and_then(|s| s.parse::<u32>().ok()).unwrap_or(64);
                    let found = self.ctx.db.with_snapshot(|snap| {
                        let ch = snap.characters.get(&char_id)?;
                        let blob = snap.service_state_blobs.get(&format!("portrait:{char_id}:{size}"))
                            .or_else(|| snap.service_state_blobs.get(&format!("portrait:{char_id}")))
                            .cloned();
                        Some((ch.bloodline_id, blob))
                    });
                    let Some((_bl, blob_opt)) = found else {
                        return self.build_error_response(
                            &client_key,
                            now,
                            req_id,
                            compat_date,
                            404,
                            format!("Character {char_id} not found"),
                            true,
                        );
                    };
                    let Some(blob) = blob_opt else {
                        return self.build_error_response(
                            &client_key,
                            now,
                            req_id,
                            compat_date,
                            404,
                            "Portrait not found",
                            true,
                        );
                    };
                    let img_body = if blob.len() % 2 == 0 && blob.chars().all(|c| c.is_ascii_hexdigit()) {
                        (0..blob.len())
                            .step_by(2)
                            .filter_map(|i| u8::from_str_radix(&blob[i..i + 2], 16).ok())
                            .collect::<Vec<u8>>()
                    } else {
                        blob.into_bytes()
                    };
                    let mut headers = HashMap::new();
                    headers.insert("Content-Type".to_string(), "image/jpeg".to_string());
                    Self::attach_esi_base_headers(
                        &mut headers,
                        remain_initial,
                        reset_initial,
                        req_id,
                        compat_date,
                    );
                    return EsiHttpResponse {
                        status_code: 200,
                        headers,
                        body: img_body,
                    };
                }
                p if p.starts_with("/cdn/achievements/v24.1.") && p.ends_with(".json") => {
                    let mgr = &self.ctx.quasar_gateway.progression.achievements;
                    let expected = format!(
                        "/cdn/achievements/v24.1.{}/{}.json",
                        mgr.sde_achievement_count, mgr.sde_achievement_hash
                    );
                    if p != expected {
                        return self.build_error_response(
                            &client_key,
                            now,
                            req_id,
                            compat_date,
                            404,
                            "Achievement definition bundle not found",
                            true,
                        );
                    }
                    return self.finalize_cached_response(
                        req,
                        now,
                        req_id,
                        compat_date,
                        remain_initial,
                        reset_initial,
                        200,
                        mgr.sde_definitions_json.clone(),
                        3600,
                        false,
                        false,
                        None,
                    );
                }
                _ => {}
            }
        }

        let Some(matched) = spec.match_operation(&req.method, raw_path) else {
            if spec.path_exists(raw_path) {
                return self.build_error_response(
                    &client_key,
                    now,
                    req_id,
                    compat_date,
                    405,
                    format!("Method {} not allowed for {}", req.method, raw_path),
                    true,
                );
            }
            return self.build_error_response(
                &client_key,
                now,
                req_id,
                compat_date,
                404,
                format!("No ESI route matches {} {}", req.method, raw_path),
                true,
            );
        };

        let op = matched.operation;

        // Parse and validate optional `?page=` parameter (1-indexed).
        let page = match query_params.get("page") {
            Some(raw_page) => match raw_page.parse::<usize>() {
                Ok(p) if p >= 1 => p,
                _ => {
                    return self.build_error_response(
                        &client_key,
                        now,
                        req_id,
                        compat_date,
                        400,
                        format!("Invalid page parameter '{raw_page}': must be an integer >= 1"),
                        true,
                    );
                }
            },
            None => 1,
        };

        // Enforce OAuth2 Bearer authentication, required scopes, and character/corporation ownership.
        let is_authenticated_route = !op.required_scopes.is_empty();
        let verified_claims = if is_authenticated_route {
            let Some(token) = extract_bearer_token(req, &query_params) else {
                return self.build_error_response(
                    &client_key,
                    now,
                    req_id,
                    compat_date,
                    401,
                    "Authorization Bearer token is required for this endpoint",
                    true,
                );
            };
            let claims = match self.ctx.db.verify_sso_access_token(&token, now) {
                Ok(c) => c,
                Err(e) => {
                    return self.build_error_response(
                        &client_key,
                        now,
                        req_id,
                        compat_date,
                        401,
                        format!("Invalid or expired SSO access token: {e}"),
                        true,
                    );
                }
            };
            for req_scope in &op.required_scopes {
                if !claims.scp.iter().any(|s| s == req_scope) {
                    return self.build_error_response(
                        &client_key,
                        now,
                        req_id,
                        compat_date,
                        403,
                        format!("token is missing required scope: {req_scope}"),
                        true,
                    );
                }
            }
            if let Some(raw_cid) = matched.path_params.get("character_id") {
                let Ok(path_char_id) = raw_cid.parse::<i64>() else {
                    return self.build_error_response(
                        &client_key,
                        now,
                        req_id,
                        compat_date,
                        400,
                        format!("Invalid character_id '{raw_cid}'"),
                        true,
                    );
                };
                if claims.character_id != path_char_id {
                    return self.build_error_response(
                        &client_key,
                        now,
                        req_id,
                        compat_date,
                        403,
                        format!(
                            "Token character_id {} is not authorized for character_id {}",
                            claims.character_id, path_char_id
                        ),
                        true,
                    );
                }
            }
            if let Some(raw_corpid) = matched.path_params.get("corporation_id") {
                let Ok(path_corp_id) = raw_corpid.parse::<i64>() else {
                    return self.build_error_response(
                        &client_key,
                        now,
                        req_id,
                        compat_date,
                        400,
                        format!("Invalid corporation_id '{raw_corpid}'"),
                        true,
                    );
                };
                let mut required_role_mask = eve_db::corp_roles::ROLE_DIRECTOR
                    | required_corp_roles_for_route(
                        matched.operation.method.as_str(),
                        matched.operation.path_pattern.as_str(),
                    );
                for role_name in &op.required_roles {
                    required_role_mask |= match role_name.as_str() {
                        "Director" => eve_db::corp_roles::ROLE_DIRECTOR,
                        "Accountant" => eve_db::corp_roles::ROLE_ACCOUNTANT,
                        "Junior_Accountant" => eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT,
                        "Trader" => eve_db::corp_roles::ROLE_TRADER,
                        "Factory_Manager" => eve_db::corp_roles::ROLE_FACTORY_MANAGER,
                        "Station_Manager" => eve_db::corp_roles::ROLE_STATION_MANAGER,
                        _ => 0,
                    };
                }
                let (char_corp, has_corp_role) = self.ctx.db.with_snapshot(|snap| {
                    let c_corp = snap
                        .characters
                        .get(&claims.character_id)
                        .map(|c| c.corporation_id);
                    let allowed_mask = required_role_mask;
                    let has_role = snap
                        .corporations
                        .get(&path_corp_id)
                        .map(|corp| {
                            corp.ceo_id == claims.character_id
                                || (corp
                                    .member_roles
                                    .get(&claims.character_id)
                                    .copied()
                                    .unwrap_or(0)
                                    & allowed_mask)
                                    != 0
                        })
                        .unwrap_or(false)
                        || snap
                            .corp_role_assignments
                            .get(&(path_corp_id, claims.character_id))
                            .map(|r| {
                                ((r.roles | r.roles_at_hq | r.roles_at_base | r.roles_at_other)
                                    & allowed_mask)
                                    != 0
                            })
                            .unwrap_or(false);
                    (c_corp, has_role)
                });
                if char_corp != Some(path_corp_id) || !has_corp_role {
                    return self.build_error_response(
                        &client_key,
                        now,
                        req_id,
                        compat_date,
                        403,
                        format!(
                            "Token character {} lacks required corporation role for {} {}",
                            claims.character_id, op.method, op.path_pattern
                        ),
                        true,
                    );
                }
            }
            if let Some(raw_alid) = matched.path_params.get("alliance_id") {
                if let Ok(path_alliance_id) = raw_alid.parse::<i64>() {
                    let forbidden = self.ctx.db.with_snapshot(|snap| {
                        let Some(al) = snap.alliances.get(&path_alliance_id) else {
                            return false;
                        };
                        let char_al = snap.characters.get(&claims.character_id).and_then(|c| c.alliance_id);
                        let char_corp = snap.characters.get(&claims.character_id).map(|c| c.corporation_id).unwrap_or(0);
                        let in_alliance = char_al == Some(path_alliance_id) || al.member_corp_ids.contains(&char_corp);
                        !in_alliance && (!al.member_corp_ids.is_empty() || char_al.is_some())
                    });
                    if forbidden {
                        return self.build_error_response(
                            &client_key,
                            now,
                            req_id,
                            compat_date,
                            403,
                            format!(
                                "Token character {} is not a member of alliance {}",
                                claims.character_id, path_alliance_id
                            ),
                            true,
                        );
                    }
                }
            }
            Some(claims)
        } else {
            None
        };

        // Check required query parameters declared in the OpenAPI operation spec.
        for param in &op.parameters {
            if param.location == "query" && param.required && !query_params.contains_key(&param.name)
            {
                return self.build_error_response(
                    &client_key,
                    now,
                    req_id,
                    compat_date,
                    400,
                    format!("Missing required query parameter '{}'", param.name),
                    true,
                );
            }
        }

        let exec: RouteExecutionResult = super::fleet_routes::dispatch_fleet_routes(
            &self.ctx,
            &op.method,
            &op.path_pattern,
            &matched.path_params,
            &query_params,
            verified_claims.as_ref(),
            &req.body,
        )
        .unwrap_or_else(|| {
            sample_routes::dispatch_operation(
                &self.ctx,
                self.started_at_unix,
                &op.method,
                &op.path_pattern,
                &matched.path_params,
                &query_params,
                verified_claims.as_ref(),
                page,
                &req.body,
            )
        });

        if exec.status_code >= 400 {
            let count_error = (400..500).contains(&exec.status_code);
            let msg = exec
                .body
                .as_ref()
                .and_then(|v| v.get("error"))
                .and_then(|v| v.as_str())
                .unwrap_or("ESI error")
                .to_string();
            return self.build_error_response(
                &client_key,
                now,
                req_id,
                compat_date,
                exec.status_code,
                msg,
                count_error,
            );
        }

        let code_key = exec.status_code.to_string();
        let needs_content_lang = op
            .responses
            .get(&code_key)
            .map(|r| {
                r.required_headers
                    .iter()
                    .any(|h| h.eq_ignore_ascii_case("Content-Language"))
            })
            .unwrap_or(false);
        let needs_x_pages = op
            .responses
            .get(&code_key)
            .map(|r| {
                r.required_headers
                    .iter()
                    .any(|h| h.eq_ignore_ascii_case("X-Pages"))
            })
            .unwrap_or(false);
        let total_pages = if needs_x_pages {
            Some(exec.total_pages.unwrap_or(1))
        } else {
            exec.total_pages
        };

        let body_bytes = match exec.body {
            Some(val) => serde_json::to_vec(&val).unwrap_or_default(),
            None => Vec::new(),
        };

        self.finalize_cached_response(
            req,
            now,
            req_id,
            compat_date,
            remain_initial,
            reset_initial,
            exec.status_code,
            body_bytes,
            exec.max_age_secs,
            is_authenticated_route,
            needs_content_lang,
            total_pages,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn finalize_cached_response(
        &self,
        req: &EsiHttpRequest,
        now: u64,
        req_id: u64,
        compat_date: &str,
        remain: u32,
        reset: u64,
        status_code: u16,
        body: Vec<u8>,
        max_age_secs: u64,
        is_private: bool,
        needs_content_lang: bool,
        total_pages: Option<usize>,
    ) -> EsiHttpResponse {
        let etag = compute_etag(&body);
        let last_modified = format_http_date(now);
        let expires = format_http_date(now.saturating_add(max_age_secs));
        let visibility = if is_private { "private" } else { "public" };
        let cache_control = format!("{visibility}, max-age={max_age_secs}");

        let mut headers = HashMap::new();
        headers.insert("ETag".to_string(), etag.clone());
        headers.insert("Cache-Control".to_string(), cache_control);
        headers.insert("Last-Modified".to_string(), last_modified);
        headers.insert("Expires".to_string(), expires);
        if needs_content_lang {
            let lang = req
                .header("accept-language")
                .filter(|l| {
                    matches!(
                        *l,
                        "en" | "de" | "fr" | "ja" | "ru" | "zh" | "ko" | "es"
                    )
                })
                .unwrap_or("en");
            headers.insert("Content-Language".to_string(), lang.to_string());
        }
        if let Some(tp) = total_pages {
            headers.insert("X-Pages".to_string(), tp.max(1).to_string());
        }
        Self::attach_esi_base_headers(&mut headers, remain, reset, req_id, compat_date);

        // Conditional GET / HEAD: return 304 Not Modified with empty body when If-None-Match matches.
        if status_code == 200 {
            if let Some(inm) = req.header("if-none-match") {
                if if_none_match_matches(inm, &etag) {
                    return EsiHttpResponse {
                        status_code: 304,
                        headers,
                        body: Vec::new(),
                    };
                }
            }
        }

        if status_code != 204 {
            headers.insert("Content-Type".to_string(), "application/json".to_string());
        }

        EsiHttpResponse {
            status_code,
            headers,
            body,
        }
    }
}

fn extract_bearer_token(
    req: &EsiHttpRequest,
    query_params: &HashMap<String, String>,
) -> Option<String> {
    if let Some(auth_hdr) = req.header("authorization") {
        let trimmed = auth_hdr.trim();
        if let Some(rest) = trimmed
            .strip_prefix("Bearer ")
            .or_else(|| trimmed.strip_prefix("bearer "))
        {
            let tok = rest.trim();
            if !tok.is_empty() {
                return Some(tok.to_string());
            }
        }
        return None;
    }
    query_params
        .get("token")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn split_path_and_query(raw_target: &str) -> (&str, HashMap<String, String>) {
    let mut query_map = HashMap::new();
    let Some((path, query_str)) = raw_target.split_once('?') else {
        return (raw_target, query_map);
    };
    for pair in query_str.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let dk = percent_decode(k);
        let dv = percent_decode(v);
        if !dk.is_empty() {
            query_map.insert(dk, dv);
        }
    }
    (path, query_map)
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let (Some(&b1), Some(&b2)) = (bytes.get(i + 1), bytes.get(i + 2)) {
                if let (Some(hi), Some(lo)) = (hex_val(b1), hex_val(b2)) {
                    out.push((hi << 4) | lo);
                    i += 3;
                    continue;
                }
            }
        } else if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn required_corp_roles_for_route(method: &str, path_pattern: &str) -> u64 {
    use eve_db::corp_roles::*;
    match (method, path_pattern) {
        ("GET", "/corporation/{corporation_id}/mining/extractions")
        | ("GET", "/corporations/{corporation_id}/structures") => ROLE_STATION_MANAGER,
        ("GET", "/corporation/{corporation_id}/mining/observers")
        | ("GET", "/corporation/{corporation_id}/mining/observers/{observer_id}") => {
            ROLE_ACCOUNTANT
        }
        ("GET", "/corporations/{corporation_id}/orders")
        | ("GET", "/corporations/{corporation_id}/orders/history") => {
            ROLE_ACCOUNTANT | ROLE_TRADER
        }
        ("GET", "/corporations/{corporation_id}/industry/jobs")
        | ("GET", "/corporations/{corporation_id}/facilities") => ROLE_FACTORY_MANAGER,
        ("GET", "/corporations/{corporation_id}/wallets")
        | ("GET", "/corporations/{corporation_id}/wallets/{division}/journal")
        | ("GET", "/corporations/{corporation_id}/wallets/{division}/transactions") => {
            ROLE_ACCOUNTANT | ROLE_JUNIOR_ACCOUNTANT
        }
        _ => ROLE_DIRECTOR,
    }
}
