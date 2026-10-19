//! Integration tests for the ESI HTTP framework (`ESI-2`):
//! - OpenAPI 3.0.2 / 3.1.0 specification loader & response schema validator (`OpenApiSpec`)
//! - HTTP/1.1 listener & router (`EsiServer`, `EsiRouter`) with versioned/unversioned paths
//! - OAuth2 PKCE JWT Bearer authentication, scope checks, and character/corporation ownership guards
//! - Deterministic `ETag` / `If-None-Match` -> `304 Not Modified` & `Cache-Control` / `Expires`
//! - `X-Pages` / `?page=` pagination with out-of-range handling
//! - Per-client `X-Esi-Error-Limit-Remain` / `X-Esi-Error-Limit-Reset` / `420 Error Limited` window
//! - Sample routes across all 31 OpenAPI tags backed by `SdeRegistry` and `WorldRepository`

#![forbid(unsafe_code)]

use eve_db::{
    AllianceRecord, ContractRecord, CorporationRecord, FleetRecord, InventoryFlag,
    InventoryItemRecord, IskCents, MailRecord, SavedFittingRecord, WarRecord,
};
use eve_server::esi::openapi::{validate_value_against_schema, OpenApiSpec};
use eve_server::esi::{
    compute_etag, if_none_match_matches, send_raw_esi_request, EsiHttpRequest, EsiHttpResponse,
    EsiRouter, EsiServer,
};
use eve_server::router::ServerContext;
use serde_json::json;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token_with_scopes(ctx: &ServerContext, character_id: i64, scopes: Vec<String>) -> String {
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            character_id,
            "evomu-esi-test",
            "https://localhost/callback",
            scopes,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    let bundle = ctx
        .db
        .exchange_pkce_code(&code, "evomu-esi-test", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code");
    bundle.access_token
}

#[tokio::test]
async fn test_esi_http_server_routing_versioning_meta_and_openapi_spec() {
    // parity: feat:esi.core.meta, feat:esi.status.status
    // parity: esi:GET /status, esi:GET /meta/compatibility-dates
    let ctx = Arc::new(ServerContext::new());
    let (addr, _router, _handle) = EsiServer::spawn(Arc::clone(&ctx), "127.0.0.1:0")
        .await
        .expect("spawn ESI HTTP server");

    let spec = OpenApiSpec::pinned();
    assert_eq!(
        spec.tags.len(),
        31,
        "ESI OpenAPI spec must define 31 tags"
    );

    // 1. Live HTTP GET /status/ across version prefixes (/latest, /v1, /v2, /legacy, /dev, unversioned)
    let expected_build = eve_machonet::VersionExchange::default()
        .build_version
        .to_string();
    for prefix in ["", "/latest", "/v1", "/v2", "/legacy", "/dev"] {
        let path = format!("{prefix}/status/");
        let req = EsiHttpRequest::new("GET", &path);
        let resp = send_raw_esi_request(addr, &req).await.expect("GET status");
        assert_eq!(resp.status_code, 200, "Failed on path {path}");
        spec.validate_response("GET", &path, resp.status_code, &resp.headers, &resp.body)
            .unwrap_or_else(|e| panic!("Schema validation failed for {path}: {e}"));
        let body = resp.json().expect("status JSON");
        assert_eq!(body["players"], json!(0));
        assert_eq!(body["vip"], json!(false));
        assert_eq!(body["server_version"], json!(expected_build));
        let start_time = body["start_time"].as_str().expect("start_time str");
        assert!(start_time.ends_with('Z') && start_time.contains('T'));
    }

    // 2. Meta endpoints: /swagger.json, /openapi.json, /versions/, /status.json, /meta/compatibility-dates
    let swagger_resp = send_raw_esi_request(
        addr,
        &EsiHttpRequest::new("GET", "/latest/swagger.json"),
    )
    .await
    .expect("GET /latest/swagger.json");
    assert_eq!(swagger_resp.status_code, 200);
    let swagger_json = swagger_resp.json().expect("swagger JSON");
    assert_eq!(swagger_json["openapi"], json!("3.1.0"));

    let versions_resp = send_raw_esi_request(addr, &EsiHttpRequest::new("GET", "/versions/"))
        .await
        .expect("GET /versions/");
    assert_eq!(versions_resp.status_code, 200);
    assert_eq!(
        versions_resp.json().expect("versions json"),
        json!(["latest", "v1", "v2", "v3", "v4", "v5", "v6", "dev", "legacy"])
    );

    let status_json_resp = send_raw_esi_request(
        addr,
        &EsiHttpRequest::new("GET", "/status.json?version=latest"),
    )
    .await
    .expect("GET /status.json");
    assert_eq!(status_json_resp.status_code, 200);
    let status_list = status_json_resp.json().expect("status.json");
    let status_arr = status_list.as_array().expect("status.json array");
    assert!(
        status_arr.len() >= 190,
        "/status.json must list route health objects"
    );
    assert!(
        status_arr
            .iter()
            .any(|entry| entry["route"] == json!("/status") && entry["status"] == json!("green")),
        "/status.json must report green for implemented /status route"
    );
    for unimplemented_route in [
        "/fleets/{fleet_id}/wings",
        "/characters/{character_id}/calendar",
        "/characters/{character_id}/contacts",
        "/characters/{character_id}/contracts/{contract_id}/items",
        "/fw/wars",
        "/incursions",
        "/industry/facilities",
        "/ui/autopilot/waypoint",
    ] {
        assert!(
            status_arr.iter().any(|entry| {
                entry["route"] == json!(unimplemented_route) && entry["status"] == json!("red")
            }),
            "/status.json must report red for unimplemented route {unimplemented_route}"
        );
    }

    let compat_resp = send_raw_esi_request(
        addr,
        &EsiHttpRequest::new("GET", "/meta/compatibility-dates"),
    )
    .await
    .expect("GET /meta/compatibility-dates");
    assert_eq!(compat_resp.status_code, 200);
    spec.validate_response(
        "GET",
        "/meta/compatibility-dates",
        compat_resp.status_code,
        &compat_resp.headers,
        &compat_resp.body,
    )
    .expect("validate /meta/compatibility-dates");
    let compat_body = compat_resp.json().expect("compat json");
    assert_eq!(
        compat_body["compatibility_dates"],
        json!(spec.compatibility_dates)
    );

    // 3. X-Compatibility-Date & datasource / X-Tenant validation
    let bad_compat = send_raw_esi_request(
        addr,
        &EsiHttpRequest::new("GET", "/status/").with_header("X-Compatibility-Date", "not-a-date"),
    )
    .await
    .expect("bad compatibility date");
    assert_eq!(bad_compat.status_code, 400);

    let unknown_compat_enum = send_raw_esi_request(
        addr,
        &EsiHttpRequest::new("GET", "/status/").with_header("X-Compatibility-Date", "2099-01-01"),
    )
    .await
    .expect("unknown compatibility date enum");
    assert_eq!(unknown_compat_enum.status_code, 400);

    let good_compat = send_raw_esi_request(
        addr,
        &EsiHttpRequest::new("GET", "/status/")
            .with_header("X-Compatibility-Date", &spec.compatibility_dates[0])
            .with_header("X-Tenant", "tranquility"),
    )
    .await
    .expect("good compatibility date");
    assert_eq!(good_compat.status_code, 200);

    let bad_datasource = send_raw_esi_request(
        addr,
        &EsiHttpRequest::new("GET", "/status/?datasource=singularity"),
    )
    .await
    .expect("bad datasource");
    assert_eq!(bad_datasource.status_code, 400);

    let bad_tenant = send_raw_esi_request(
        addr,
        &EsiHttpRequest::new("GET", "/status/").with_header("X-Tenant", "singularity"),
    )
    .await
    .expect("bad tenant");
    assert_eq!(bad_tenant.status_code, 400);

    // 4. 405 Method Not Allowed, 404 Not Found, and 501 Not Implemented
    let method_not_allowed =
        send_raw_esi_request(addr, &EsiHttpRequest::new("DELETE", "/status/"))
            .await
            .expect("DELETE /status/");
    assert_eq!(method_not_allowed.status_code, 405);

    let compat_method_not_allowed = send_raw_esi_request(
        addr,
        &EsiHttpRequest::new("POST", "/meta/compatibility-dates"),
    )
    .await
    .expect("POST /meta/compatibility-dates");
    assert_eq!(compat_method_not_allowed.status_code, 405);

    let unknown_path = send_raw_esi_request(
        addr,
        &EsiHttpRequest::new("GET", "/v1/nonexistent/endpoint"),
    )
    .await
    .expect("GET unknown path");
    assert_eq!(unknown_path.status_code, 404);

    let token = issue_token_with_scopes(
        &ctx,
        90_000_001,
        vec!["esi-fleets.write_fleet.v1".to_string()],
    );
    let unimplemented_op = send_raw_esi_request(
        addr,
        &EsiHttpRequest::new("POST", "/v1/fleets/100/wings/")
            .with_bearer_token(&token)
            .with_body(b"{}".to_vec()),
    )
    .await
    .expect("POST unimplemented operation");
    assert_eq!(unimplemented_op.status_code, 501);

    let verify_resp = send_raw_esi_request(
        addr,
        &EsiHttpRequest::new("GET", "/verify/").with_bearer_token(&token),
    )
    .await
    .expect("GET /verify/");
    assert_eq!(verify_resp.status_code, 200);
    assert_eq!(
        verify_resp.json().expect("verify json")["CharacterID"],
        json!(90_000_001)
    );

    // 7. Oversized Content-Length (> 2 MiB) rejected with 413 Payload Too Large (R53-P1-1)
    let oversized_resp = send_raw_esi_request(
        addr,
        &EsiHttpRequest::new("POST", "/v1/universe/names/")
            .with_header("Content-Length", "3000000"),
    )
    .await
    .expect("POST with oversized Content-Length");
    assert_eq!(oversized_resp.status_code, 413);
}

#[tokio::test]
async fn test_esi_oauth2_bearer_sso_scopes_character_ownership_and_verify() {
    // parity: feat:esi.core.meta, feat:esi.wallet.char-balance
    // parity: esi:GET /characters/{character_id}/wallet
    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));

    let wallet_token = issue_token_with_scopes(
        &ctx,
        90_000_001,
        vec!["esi-wallet.read_character_wallet.v1".to_string()],
    );
    ctx.advance_virtual_clock(5);

    // 1. /verify/ returns character identity and granted scopes for valid Bearer token
    let verify_resp = router.handle_request(
        &EsiHttpRequest::new("GET", "/verify/").with_bearer_token(&wallet_token),
    );
    assert_eq!(verify_resp.status_code, 200);
    let verify_json = verify_resp.json().expect("verify json");
    assert_eq!(verify_json["CharacterID"], json!(90_000_001));
    assert_eq!(
        verify_json["Scopes"],
        json!("esi-wallet.read_character_wallet.v1")
    );
    assert_eq!(verify_json["TokenType"], json!("Character"));

    // 2. Missing token on authenticated endpoint -> 401 Unauthorized
    let no_token_resp = router.handle_request(&EsiHttpRequest::new(
        "GET",
        "/v1/characters/90000001/wallet/",
    ));
    assert_eq!(no_token_resp.status_code, 401);

    // 3. Tampered JWT signature -> 401 Unauthorized
    let tampered_token = format!("{wallet_token}tampered");
    let tampered_resp = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/wallet/")
            .with_bearer_token(&tampered_token),
    );
    assert_eq!(tampered_resp.status_code, 401);

    // 4. Insufficient scope (wallet token used on skills endpoint) -> 403 Forbidden
    let missing_scope_resp = router.handle_request(
        &EsiHttpRequest::new("GET", "/v4/characters/90000001/skills/")
            .with_bearer_token(&wallet_token),
    );
    assert_eq!(missing_scope_resp.status_code, 403);

    // 5. Character ID mismatch (character 90_000_001 token used for character 90_000_002) -> 403 Forbidden
    let wrong_char_resp = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/characters/90000002/wallet/")
            .with_bearer_token(&wallet_token),
    );
    assert_eq!(wrong_char_resp.status_code, 403);

    // 6. Valid Bearer header & valid ?token= query parameter -> 200 OK with exact balance
    let expected_balance = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&90_000_001)
            .expect("char 90_000_001")
            .balance
            .to_wire_f64()
    });
    let ok_header_resp = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/wallet/")
            .with_bearer_token(&wallet_token),
    );
    assert_eq!(ok_header_resp.status_code, 200);
    OpenApiSpec::pinned()
        .validate_response(
            "GET",
            "/v1/characters/90000001/wallet/",
            ok_header_resp.status_code,
            &ok_header_resp.headers,
            &ok_header_resp.body,
        )
        .expect("validate wallet response");
    assert_eq!(
        ok_header_resp.json().expect("wallet json"),
        json!(expected_balance)
    );

    let ok_query_resp = router.handle_request(&EsiHttpRequest::new(
        "GET",
        format!("/v1/characters/90000001/wallet/?token={wallet_token}"),
    ));
    assert_eq!(ok_query_resp.status_code, 200);
    assert_eq!(
        ok_query_resp.json().expect("wallet query json"),
        json!(expected_balance)
    );

    // 7. Account kick invalidates otherwise unexpired tokens -> 401 Unauthorized
    let now = ctx.now_unix();
    ctx.db.kick_user(1, "ESI kick check", now);
    let kicked_resp = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/wallet/")
            .with_bearer_token(&wallet_token),
    );
    assert_eq!(kicked_resp.status_code, 401);

    // 8. Expired token -> 401 Unauthorized (advance clock 1s past kick so fresh token is valid first)
    ctx.advance_virtual_clock(1);
    let fresh_token = issue_token_with_scopes(
        &ctx,
        90_000_001,
        vec!["esi-wallet.read_character_wallet.v1".to_string()],
    );
    let fresh_ok = router.handle_request(
        &EsiHttpRequest::new(
            "GET",
            "/v1/characters/90000001/wallet/?datasource=tranquilit%79",
        )
        .with_bearer_token(&fresh_token),
    );
    assert_eq!(fresh_ok.status_code, 200);

    // 9. Banned account -> 401 Unauthorized
    let now_ban = ctx.now_unix();
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.get_mut("demo") {
            acc.banned_until_unix = Some(now_ban + 3600);
            acc.ban_reason = Some("Test ban".to_string());
        }
        Ok(())
    });
    let banned_resp = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/wallet/")
            .with_bearer_token(&fresh_token),
    );
    assert_eq!(banned_resp.status_code, 401);
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.get_mut("demo") {
            acc.banned_until_unix = None;
            acc.ban_reason = None;
        }
        Ok(())
    });

    ctx.advance_virtual_clock(10_000);
    let expired_resp = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/wallet/")
            .with_bearer_token(&fresh_token),
    );
    assert_eq!(expired_resp.status_code, 401);
}

#[tokio::test]
async fn test_esi_etag_caching_304_not_modified_and_pagination_x_pages() {
    // parity: esi:GET /universe/types, esi:GET /characters/{character_id}/wallet
    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let wallet_token = issue_token_with_scopes(
        &ctx,
        90_000_001,
        vec!["esi-wallet.read_character_wallet.v1".to_string()],
    );

    // 1. Initial GET returns 200 OK with ETag, Cache-Control, Expires, Last-Modified
    let first_resp = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/wallet/")
            .with_bearer_token(&wallet_token),
    );
    assert_eq!(first_resp.status_code, 200);
    let etag = first_resp
        .header("ETag")
        .expect("ETag header must be present")
        .to_string();
    assert_eq!(etag, compute_etag(&first_resp.body));
    assert!(if_none_match_matches(&etag, &etag));
    assert!(if_none_match_matches(&format!("W/{etag}"), &etag));
    assert!(if_none_match_matches("*", &etag));
    assert!(first_resp.header("Cache-Control").is_some());
    assert!(first_resp.header("Expires").is_some());
    assert!(first_resp.header("Last-Modified").is_some());

    // 2. Conditional GET with matching If-None-Match returns 304 Not Modified and empty body
    let cached_resp = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/wallet/")
            .with_bearer_token(&wallet_token)
            .with_header("If-None-Match", &etag),
    );
    assert_eq!(cached_resp.status_code, 304);
    assert!(
        cached_resp.body.is_empty(),
        "304 Not Modified response must have an empty body"
    );
    assert_eq!(cached_resp.header("ETag"), Some(etag.as_str()));
    spec.validate_response(
        "GET",
        "/v1/characters/90000001/wallet/",
        cached_resp.status_code,
        &cached_resp.headers,
        &cached_resp.body,
    )
    .expect("304 response must pass OpenAPI validation");

    // 3. Mutating underlying wallet balance changes ETag and returns 200 OK
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.balance = IskCents::from_isk(9_876_543);
        }
    });
    let updated_resp = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/wallet/")
            .with_bearer_token(&wallet_token)
            .with_header("If-None-Match", &etag),
    );
    assert_eq!(updated_resp.status_code, 200);
    assert_ne!(updated_resp.header("ETag"), Some(etag.as_str()));
    assert_eq!(
        updated_resp.json().expect("updated wallet json"),
        json!(9_876_543.0)
    );

    // 4. Pagination (?page=1, ?page=2, X-Pages header, out-of-range page)
    let page1_resp = router.handle_request(&EsiHttpRequest::new(
        "GET",
        "/v1/universe/types/?page=1",
    ));
    assert_eq!(page1_resp.status_code, 200);
    spec.validate_response(
        "GET",
        "/v1/universe/types/",
        page1_resp.status_code,
        &page1_resp.headers,
        &page1_resp.body,
    )
    .expect("validate /v1/universe/types/ page 1");
    let x_pages: usize = page1_resp
        .header("X-Pages")
        .expect("X-Pages header")
        .parse()
        .expect("parse X-Pages");
    assert!(
        x_pages >= 20,
        "SDE types must span at least 20 pages of 1000 items"
    );
    let page1_ids: Vec<i64> = serde_json::from_slice(&page1_resp.body).expect("page 1 ids");
    assert_eq!(page1_ids.len(), 1000);

    let page2_resp = router.handle_request(&EsiHttpRequest::new(
        "GET",
        "/v1/universe/types/?page=2",
    ));
    assert_eq!(page2_resp.status_code, 200);
    let page2_ids: Vec<i64> = serde_json::from_slice(&page2_resp.body).expect("page 2 ids");
    assert_eq!(page2_ids.len(), 1000);
    assert_ne!(
        page1_ids[0], page2_ids[0],
        "Page 1 and Page 2 must be distinct"
    );

    let page0_resp = router.handle_request(&EsiHttpRequest::new(
        "GET",
        "/v1/universe/types/?page=0",
    ));
    assert_eq!(page0_resp.status_code, 400);

    let out_of_range_resp = router.handle_request(&EsiHttpRequest::new(
        "GET",
        format!("/v1/universe/types/?page={}", x_pages + 10),
    ));
    assert_eq!(out_of_range_resp.status_code, 404);
}

#[tokio::test]
async fn test_esi_error_limit_window_headers_and_420_throttling() {
    // parity: feat:esi.core.rate-limits
    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::with_error_limit(Arc::clone(&ctx), 3, 60);

    // 3 client errors from 10.0.0.1 consume 10.0.0.1's 3-error budget in the current 60s window
    for expected_remain in ["2", "1", "0"] {
        let err_resp = router.handle_request(
            &EsiHttpRequest::new("GET", "/v1/unknown/path").with_peer_ip("10.0.0.1"),
        );
        assert_eq!(err_resp.status_code, 404);
        assert_eq!(
            err_resp.header("X-Esi-Error-Limit-Remain"),
            Some(expected_remain)
        );
        assert!(err_resp.header("X-Esi-Error-Limit-Reset").is_some());
        assert!(err_resp.header("X-Esi-Request-Id").is_some());
    }

    // 4th request from 10.0.0.1 in the same window is throttled with 420 Error Limited
    let throttled =
        router.handle_request(&EsiHttpRequest::new("GET", "/status/").with_peer_ip("10.0.0.1"));
    assert_eq!(throttled.status_code, 420);
    assert_eq!(throttled.header("X-Esi-Error-Limit-Remain"), Some("0"));
    let throttled_body = throttled.json().expect("420 json");
    assert_eq!(throttled_body["status"], json!(420));

    // A separate client IP (10.0.0.2) is NOT throttled by 10.0.0.1's exhausted error budget
    let other_client =
        router.handle_request(&EsiHttpRequest::new("GET", "/status/").with_peer_ip("10.0.0.2"));
    assert_eq!(other_client.status_code, 200);
    assert_eq!(other_client.header("X-Esi-Error-Limit-Remain"), Some("3"));

    // Advancing clock into the next 60-second window resets 10.0.0.1's error budget
    ctx.advance_virtual_clock(65);
    let recovered =
        router.handle_request(&EsiHttpRequest::new("GET", "/status/").with_peer_ip("10.0.0.1"));
    assert_eq!(recovered.status_code, 200);
    assert_eq!(recovered.header("X-Esi-Error-Limit-Remain"), Some("3"));
    assert_eq!(
        recovered.header("Cache-Control"),
        Some("public, max-age=30")
    );

    // Verify per-route cache timers and versioned route prefixes (/latest, /dev, /legacy, /vN)
    for prefix in ["/latest/status/", "/dev/status/", "/legacy/status/", "/v1/status/"] {
        let v_resp = router.handle_request(&EsiHttpRequest::new("GET", prefix));
        assert_eq!(v_resp.status_code, 200);
        assert_eq!(
            v_resp.header("Cache-Control"),
            Some("public, max-age=30")
        );
    }
    let types_resp = router.handle_request(&EsiHttpRequest::new("GET", "/v1/universe/types/"));
    assert_eq!(types_resp.status_code, 200);
    assert_eq!(
        types_resp.header("Cache-Control"),
        Some("public, max-age=3600")
    );
}

#[tokio::test]
async fn test_esi_all_31_openapi_tags_sample_routes_and_schema_validation() {
    // parity: feat:esi.status.status, feat:esi.wallet.char-balance, feat:esi.skills.skills
    // parity: feat:esi.wars.list
    // parity: esi:GET /alliances, esi:GET /corporations/npccorps
    // parity: esi:GET /characters/{character_id}/standings, esi:GET /characters/{character_id}/wallet
    // parity: esi:GET /characters/{character_id}/skills, esi:GET /characters/{character_id}/clones
    // parity: esi:GET /dogma/attributes, esi:GET /dogma/effects
    // parity: esi:GET /markets/groups, esi:GET /universe/schematics/{schematic_id}
    // parity: esi:GET /universe/types, esi:GET /wars
    let ctx = Arc::new(ServerContext::new());

    // Seed rich world state so collection and detail routes exercise non-empty schemas
    let corp_id = 98_000_001_i64;
    let alliance_id = 99_000_001_i64;
    let contract_id = 4001_i64;
    let contract_item_id = 880_001_i64;
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Apex Vanguard".to_string(),
                ticker: "APEX".to_string(),
                ceo_id: 90_000_001,
                alliance_id: Some(alliance_id),
                tax_rate: 0.05,
                master_wallet: IskCents::from_isk(1_000_000),
                members: HashSet::from([90_000_001]),
                ..Default::default()
            },
        );
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "Apex Coalition".to_string(),
                short_name: "APEXC".to_string(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );
        snap.fleets.insert(
            5001,
            FleetRecord {
                fleet_id: 5001,
                boss_id: 90_000_001,
                members: vec![90_000_001],
                wings: vec![6001],
                squads: vec![7001],
                free_move: true,
                is_registered: true,
                ..Default::default()
            },
        );
        snap.saved_fittings.insert(
            1,
            SavedFittingRecord {
                fitting_id: 1,
                owner_id: 90_000_001,
                name: "Jita Rifter".to_string(),
                ship_type_id: 587,
                description: "Standard tackle fit".to_string(),
                items: vec![(2048, InventoryFlag::LoSlot0, 1)],
            },
        );
        snap.items.insert(
            contract_item_id,
            InventoryItemRecord {
                item_id: contract_item_id,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.contracts.insert(
            contract_id,
            ContractRecord {
                contract_id,
                issuer_id: 90_000_001,
                acceptor_id: None,
                contract_type: "ItemExchange".to_string(),
                status: "Outstanding".to_string(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                price: IskCents::from_isk(100_000),
                reward: IskCents(0),
                collateral: IskCents(0),
                item_ids: vec![contract_item_id],
                expiration_unix: now + 86_400,
            },
        );
        snap.mails.push(MailRecord {
            message_id: 101,
            sender_id: 90_000_001,
            recipient_id: 90_000_001,
            subject: "Welcome to ESI".to_string(),
            body: "Live snapshot mail body".to_string(),
        });
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.standings.clear();
            ch.standings.insert(1_000_125, 4.5);
        }
        snap.wars.insert(
            701,
            WarRecord {
                war_id: 701,
                aggressor_corp_id: corp_id,
                defender_corp_id: 1_000_002,
                mutual: false,
                started_unix: now + 3600,
                active: true,
            },
        );
        snap.wars.insert(
            702,
            WarRecord {
                war_id: 702,
                aggressor_corp_id: corp_id,
                defender_corp_id: 1_000_003,
                mutual: true,
                started_unix: now + 3600,
                active: true,
            },
        );
    });

    // Collect every scope needed across the OpenAPI spec for character 90_000_001
    let all_scopes = vec![
        "esi-assets.read_assets.v1".to_string(),
        "esi-calendar.read_calendar_events.v1".to_string(),
        "esi-characters.read_contacts.v1".to_string(),
        "esi-characters.read_loyalty.v1".to_string(),
        "esi-characters.read_standings.v1".to_string(),
        "esi-clones.read_clones.v1".to_string(),
        "esi-clones.read_implants.v1".to_string(),
        "esi-contracts.read_character_contracts.v1".to_string(),
        "esi-fittings.read_fittings.v1".to_string(),
        "esi-fleets.read_fleet.v1".to_string(),
        "esi-industry.read_character_jobs.v1".to_string(),
        "esi-killmails.read_killmails.v1".to_string(),
        "esi-location.read_location.v1".to_string(),
        "esi-location.read_online.v1".to_string(),
        "esi-location.read_ship_type.v1".to_string(),
        "esi-mail.read_mail.v1".to_string(),
        "esi-search.search_structures.v1".to_string(),
        "esi-skills.read_skills.v1".to_string(),
        "esi-skills.read_skillqueue.v1".to_string(),
        "esi-ui.write_waypoint.v1".to_string(),
        "esi-wallet.read_character_wallet.v1".to_string(),
    ];
    let token = issue_token_with_scopes(&ctx, 90_000_001, all_scopes);
    let wallet_only_token = issue_token_with_scopes(
        &ctx,
        90_000_001,
        vec!["esi-wallet.read_character_wallet.v1".to_string()],
    );

    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    // 24 OpenAPI tags returning 200 OK + 7 tags (Calendar, Contacts, Contracts, Faction Warfare, Incursions, Industry, User Interface) returning 501 until ESI-5..ESI-7
    let cases: Vec<(&str, &str, u16)> = vec![
        ("GET", "/v1/alliances/", 200),
        ("GET", "/v5/characters/90000001/assets/", 200),
        ("GET", "/v1/characters/90000001/calendar/", 501),
        ("GET", "/v1/characters/90000001/standings/", 200),
        ("GET", "/v3/characters/90000001/clones/", 200),
        ("GET", "/v2/characters/90000001/contacts/", 501),
        (
            "GET",
            "/v1/characters/90000001/contracts/4001/items/",
            501,
        ),
        ("GET", "/v1/corporations/npccorps/", 200),
        ("GET", "/v1/dogma/attributes/", 200),
        ("GET", "/v1/dogma/effects/", 200),
        ("GET", "/v1/fw/wars/", 501),
        ("GET", "/v2/characters/90000001/fittings/", 200),
        ("GET", "/v1/characters/90000001/fleet/", 200),
        ("GET", "/v1/incursions/", 501),
        ("GET", "/v1/industry/facilities/", 501),
        ("GET", "/v1/insurance/prices/", 200),
        ("GET", "/v1/characters/90000001/killmails/recent/", 200),
        ("GET", "/v1/characters/90000001/location/", 200),
        ("GET", "/v1/characters/90000001/loyalty/points/", 200),
        ("GET", "/v1/characters/90000001/mail/", 200),
        ("GET", "/v1/markets/groups/", 200),
        ("GET", "/meta/compatibility-dates", 200),
        ("GET", "/v1/universe/schematics/65/", 200),
        ("GET", "/v1/route/30000142/30002187/", 200),
        (
            "GET",
            "/v3/characters/90000001/search/?categories=solar_system,inventory_type&search=Jita",
            200,
        ),
        ("GET", "/v1/characters/90000001/attributes/", 200),
        ("GET", "/v4/characters/90000001/skills/", 200),
        ("GET", "/v1/sovereignty/map/", 200),
        ("GET", "/v1/status/", 200),
        ("GET", "/v1/universe/categories/", 200),
        ("GET", "/v1/universe/categories/6/", 200),
        ("GET", "/v1/universe/constellations/", 200),
        ("GET", "/v1/universe/constellations/20000020/", 200),
        ("GET", "/v1/universe/groups/", 200),
        ("GET", "/v1/universe/groups/25/", 200),
        ("GET", "/v1/universe/regions/", 200),
        ("GET", "/v1/universe/regions/10000002/", 200),
        ("GET", "/v1/universe/systems/", 200),
        ("GET", "/v4/universe/systems/30000142/", 200),
        ("GET", "/v1/universe/types/", 200),
        (
            "POST",
            "/v2/ui/autopilot/waypoint/?add_to_beginning=false&clear_other_waypoints=true&destination_id=30000142",
            501,
        ),
        ("GET", "/v1/characters/90000001/wallet/", 200),
        ("GET", "/v1/wars/", 200),
    ];

    let mut covered_tags = BTreeSet::new();
    for (method, raw_target, expected_status) in &cases {
        let req = EsiHttpRequest::new(*method, *raw_target).with_bearer_token(&token);
        let resp = router.handle_request(&req);
        let raw_path = raw_target.split('?').next().unwrap_or(raw_target);
        assert_eq!(
            resp.status_code,
            *expected_status,
            "Expected {expected_status} for {method} {raw_target}, got {} ({})",
            resp.status_code,
            String::from_utf8_lossy(&resp.body)
        );
        spec.validate_response(
            method,
            raw_path,
            resp.status_code,
            &resp.headers,
            &resp.body,
        )
        .unwrap_or_else(|e| {
            panic!("OpenAPI schema validation failed for {method} {raw_target}: {e}")
        });
        let matched = spec
            .match_operation(method, raw_path)
            .expect("matched operation");
        for tag in &matched.operation.tags {
            covered_tags.insert(tag.clone());
        }
    }

    let expected_tags: BTreeSet<String> = spec.tags.iter().cloned().collect();
    assert_eq!(
        covered_tags, expected_tags,
        "Sample routes must cover all 31 OpenAPI tags"
    );

    // Explicit value and negative assertions for every parity-tagged route:
    // 1. GET /alliances
    let alliances_resp = router.handle_request(&EsiHttpRequest::new("GET", "/v1/alliances/"));
    let alliances_ids: Vec<i64> =
        serde_json::from_slice(&alliances_resp.body).expect("alliances array");
    assert_eq!(alliances_ids, vec![alliance_id]);
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("POST", "/v1/alliances/"))
            .status_code,
        405
    );

    // 2. GET /corporations/npccorps
    let npccorps_resp =
        router.handle_request(&EsiHttpRequest::new("GET", "/v1/corporations/npccorps/"));
    let npccorps_ids: Vec<i64> =
        serde_json::from_slice(&npccorps_resp.body).expect("npccorps array");
    assert!(npccorps_ids.contains(&1_000_002));
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("DELETE", "/v1/corporations/npccorps/"))
            .status_code,
        405
    );

    // 3. GET /characters/{character_id}/standings
    let standings_resp = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/standings/")
            .with_bearer_token(&token),
    );
    let standings_json = standings_resp.json().expect("standings json");
    assert_eq!(
        standings_json,
        json!([{
            "from_id": 1_000_125,
            "from_type": "npc_corp",
            "standing": 4.5
        }])
    );
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new(
                "GET",
                "/v1/characters/90000001/standings/"
            ))
            .status_code,
        401
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", "/v1/characters/90000001/standings/")
                    .with_bearer_token(&wallet_only_token)
            )
            .status_code,
        403
    );

    // 4. GET /characters/{character_id}/skills & attributes
    let skills_resp = router.handle_request(
        &EsiHttpRequest::new("GET", "/v4/characters/90000001/skills/").with_bearer_token(&token),
    );
    let skills_json = skills_resp.json().expect("skills json");
    assert!(skills_json["total_sp"].as_i64().unwrap_or(0) > 0);
    assert!(!skills_json["skills"].as_array().unwrap().is_empty());
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new(
                "GET",
                "/v4/characters/90000001/skills/"
            ))
            .status_code,
        401
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", "/v4/characters/90000002/skills/")
                    .with_bearer_token(&token)
            )
            .status_code,
        403
    );

    let attrs_resp = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/attributes/")
            .with_bearer_token(&token),
    );
    let attrs_json = attrs_resp.json().expect("attributes json");
    assert!(attrs_json.get("accrued_remap_cooldown_date").is_none());
    assert!(attrs_json.get("last_remap_date").is_none());

    // 5. GET /characters/{character_id}/clones
    let clones_resp = router.handle_request(
        &EsiHttpRequest::new("GET", "/v3/characters/90000001/clones/").with_bearer_token(&token),
    );
    let clones_json = clones_resp.json().expect("clones json");
    assert!(clones_json["home_location"]["location_id"].as_i64().unwrap_or(0) > 0);
    assert!(clones_json["jump_clones"].is_array());
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", "/v3/characters/90000001/clones/")
                    .with_bearer_token(&wallet_only_token)
            )
            .status_code,
        403
    );

    // 6. GET /dogma/attributes, GET /dogma/effects, GET /characters/{character_id}/fleet, GET /sovereignty/map
    let dogma_attrs_resp =
        router.handle_request(&EsiHttpRequest::new("GET", "/v1/dogma/attributes/"));
    let dogma_attrs: Vec<i64> =
        serde_json::from_slice(&dogma_attrs_resp.body).expect("dogma attributes");
    assert!(dogma_attrs.contains(&9));
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("POST", "/v1/dogma/attributes/"))
            .status_code,
        405
    );

    let dogma_fx_resp = router.handle_request(&EsiHttpRequest::new("GET", "/v1/dogma/effects/"));
    let dogma_fx: Vec<i64> = serde_json::from_slice(&dogma_fx_resp.body).expect("dogma effects");
    assert!(dogma_fx.contains(&10));
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("POST", "/v1/dogma/effects/"))
            .status_code,
        405
    );

    let fleet_resp = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/fleet/").with_bearer_token(&token),
    );
    let fleet_json = fleet_resp.json().expect("fleet json");
    assert_eq!(
        fleet_json,
        json!({
            "fleet_id": 5001,
            "fleet_boss_id": 90_000_001,
            "wing_id": -1,
            "squad_id": -1,
            "role": "fleet_commander",
        })
    );

    let sov_resp = router.handle_request(&EsiHttpRequest::new("GET", "/v1/sovereignty/map/"));
    let sov_json = sov_resp.json().expect("sovereignty map json");
    let sov_arr = sov_json.as_array().expect("sovereignty map array");
    assert_eq!(sov_arr.len(), ctx.sde.solar_systems.len());
    assert!(sov_arr.iter().any(|e| {
        e["system_id"] == json!(30_000_142) && e["faction_id"] == json!(500_001)
    }));

    // 7. GET /markets/groups, GET /universe/schematics/{schematic_id}, GET /insurance/prices, GET /characters/{character_id}/mail
    let mg_resp = router.handle_request(&EsiHttpRequest::new("GET", "/v1/markets/groups/"));
    let mg_ids: Vec<i64> = serde_json::from_slice(&mg_resp.body).expect("market groups");
    assert!(!mg_ids.is_empty());
    assert!(mg_ids.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("POST", "/v1/markets/groups/"))
            .status_code,
        405
    );

    let schem_resp =
        router.handle_request(&EsiHttpRequest::new("GET", "/v1/universe/schematics/65/"));
    let schem_json = schem_resp.json().expect("schematic 65 json");
    let expected_schem = ctx
        .sde
        .planet_schematics
        .get(&65)
        .expect("schematic 65 in SDE");
    assert_eq!(
        schem_json,
        json!({
            "schematic_name": expected_schem.name,
            "cycle_time": expected_schem.cycle_time as i64,
        })
    );
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new(
                "GET",
                "/v1/universe/schematics/99999999/"
            ))
            .status_code,
        404
    );

    let ins_resp = router.handle_request(&EsiHttpRequest::new("GET", "/v1/insurance/prices/"));
    let ins_json = ins_resp.json().expect("insurance json");
    let first_levels = ins_json[0]["levels"].as_array().expect("insurance levels");
    assert_eq!(
        first_levels.len(),
        6,
        "Insurance must include all 6 wiki tiers (Basic, Standard, Bronze, Silver, Gold, Platinum)"
    );
    assert_eq!(first_levels[1]["name"], json!("Standard"));

    let mail_resp = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/mail/").with_bearer_token(&token),
    );
    let mail_json = mail_resp.json().expect("mail json");
    assert_eq!(
        mail_json,
        json!([{
            "mail_id": 101,
            "from": 90_000_001,
            "subject": "Welcome to ESI",
            "recipients": [{
                "recipient_id": 90_000_001,
                "recipient_type": "character",
            }],
        }])
    );

    // 8. GET /universe/systems/{system_id} meters check & POST /ui/autopilot/waypoint destination validation
    let jita_resp =
        router.handle_request(&EsiHttpRequest::new("GET", "/v4/universe/systems/30000142/"));
    let jita_json = jita_resp.json().expect("jita json");
    assert!(
        jita_json["position"]["x"].as_f64().unwrap_or(0.0).abs() > 1.0e15,
        "System position coordinates must be in meters, not light-years"
    );

    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new(
                    "POST",
                    "/v2/ui/autopilot/waypoint/?add_to_beginning=false&clear_other_waypoints=false&destination_id=99999999"
                )
                .with_bearer_token(&token)
            )
            .status_code,
        404
    );

    // 9. GET /wars (+ max_war_id filter and 400 on invalid max_war_id)
    let wars_resp = router.handle_request(&EsiHttpRequest::new("GET", "/v1/wars/"));
    let wars_ids: Vec<i64> = serde_json::from_slice(&wars_resp.body).expect("wars array");
    assert_eq!(wars_ids, vec![702, 701]);
    let wars_filtered =
        router.handle_request(&EsiHttpRequest::new("GET", "/v1/wars/?max_war_id=702"));
    assert_eq!(wars_filtered.status_code, 200);
    let filtered_ids: Vec<i64> =
        serde_json::from_slice(&wars_filtered.body).expect("filtered wars array");
    assert_eq!(filtered_ids, vec![701]);
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("GET", "/v1/wars/?max_war_id=invalid"))
            .status_code,
        400
    );
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("POST", "/v1/wars/"))
            .status_code,
        405
    );

    // Verify negative schema validation catches contract violations:
    // 1. Missing required field in /status
    let bad_status_resp = EsiHttpResponse {
        status_code: 200,
        headers: HashMap::from([
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Cache-Control".to_string(), "public".to_string()),
            ("ETag".to_string(), "\"abc\"".to_string()),
            (
                "Last-Modified".to_string(),
                "Sun, 06 Nov 1994 08:49:37 GMT".to_string(),
            ),
            ("X-Esi-Error-Limit-Remain".to_string(), "100".to_string()),
            ("X-Esi-Error-Limit-Reset".to_string(), "60".to_string()),
            ("X-Esi-Request-Id".to_string(), "req-1".to_string()),
        ]),
        body: br#"{"players": 5}"#.to_vec(),
    };
    assert!(
        spec.validate_response(
            "GET",
            "/status",
            bad_status_resp.status_code,
            &bad_status_resp.headers,
            &bad_status_resp.body
        )
        .is_err(),
        "Schema validator must reject /status missing required 'server_version' and 'start_time'"
    );

    // 2. Missing required ETag header
    let missing_etag_resp = EsiHttpResponse {
        status_code: 200,
        headers: HashMap::from([
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Cache-Control".to_string(), "public".to_string()),
            (
                "Last-Modified".to_string(),
                "Sun, 06 Nov 1994 08:49:37 GMT".to_string(),
            ),
            ("X-Esi-Error-Limit-Remain".to_string(), "100".to_string()),
            ("X-Esi-Error-Limit-Reset".to_string(), "60".to_string()),
            ("X-Esi-Request-Id".to_string(), "req-1".to_string()),
        ]),
        body: br#"{"players": 5, "server_version": "2301579", "start_time": "2026-01-01T00:00:00Z"}"#.to_vec(),
    };
    assert!(
        spec.validate_response(
            "GET",
            "/status",
            missing_etag_resp.status_code,
            &missing_etag_resp.headers,
            &missing_etag_resp.body
        )
        .is_err(),
        "Schema validator must reject response missing required ETag header"
    );

    // 3. Composite schema (oneOf, anyOf, allOf), OpenAPI 3.1 type array, and integer enum negative checks
    let one_of_schema = json!({
        "oneOf": [
            { "type": "integer", "minimum": 10 },
            { "type": "integer", "maximum": 20 }
        ]
    });
    assert!(validate_value_against_schema(&json!(5), &one_of_schema, "oneOf_ok").is_ok());
    assert!(
        validate_value_against_schema(&json!(15), &one_of_schema, "oneOf_both").is_err(),
        "oneOf must fail when value matches 2 subschemas"
    );
    assert!(
        validate_value_against_schema(&json!("str"), &one_of_schema, "oneOf_none").is_err(),
        "oneOf must fail when value matches 0 subschemas"
    );

    let any_of_schema = json!({
        "anyOf": [
            { "type": "string" },
            { "type": "integer" }
        ]
    });
    assert!(validate_value_against_schema(&json!("ok"), &any_of_schema, "anyOf_ok").is_ok());
    assert!(
        validate_value_against_schema(&json!(true), &any_of_schema, "anyOf_bad").is_err(),
        "anyOf must fail when value matches no subschema"
    );

    let all_of_schema = json!({
        "allOf": [
            { "type": "object", "required": ["a"], "properties": { "a": { "type": "integer" }, "b": { "type": "string" } } },
            { "type": "object", "required": ["b"], "properties": { "a": { "type": "integer" }, "b": { "type": "string" } } }
        ]
    });
    assert!(
        validate_value_against_schema(&json!({"a": 1, "b": "x"}), &all_of_schema, "allOf_ok")
            .is_ok()
    );
    assert!(
        validate_value_against_schema(&json!({"a": 1}), &all_of_schema, "allOf_missing_b").is_err(),
        "allOf must fail when any subschema fails"
    );

    let type_array_schema = json!({ "type": ["string", "null"] });
    assert!(validate_value_against_schema(&json!(null), &type_array_schema, "null_ok").is_ok());
    assert!(validate_value_against_schema(&json!("hi"), &type_array_schema, "str_ok").is_ok());
    assert!(
        validate_value_against_schema(&json!(42), &type_array_schema, "int_bad").is_err(),
        "OpenAPI 3.1 type array must reject unlisted type"
    );

    let int_enum_schema = json!({ "type": "integer", "enum": [1, 2, 4, 8] });
    assert!(validate_value_against_schema(&json!(4), &int_enum_schema, "int_enum_ok").is_ok());
    assert!(
        validate_value_against_schema(&json!(3), &int_enum_schema, "int_enum_bad").is_err(),
        "Integer enum must reject values not in enum array"
    );
}
