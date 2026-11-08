//! Unit and mutation-killing tests for `esi::router`.

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;

#[test]
fn url_query_percent_decode_hex_language_bearer_and_role_table_helpers() {
    assert_eq!(hex_val(b'0'), Some(0));
    assert_eq!(hex_val(b'9'), Some(9));
    assert_eq!(hex_val(b'a'), Some(10));
    assert_eq!(hex_val(b'f'), Some(15));
    assert_eq!(hex_val(b'A'), Some(10));
    assert_eq!(hex_val(b'F'), Some(15));
    assert_eq!(hex_val(b'g'), None);

    assert_eq!(percent_decode("hello+world%21%3F%zz%2"), "hello world!?%zz%2");

    let (path, q) = split_path_and_query("/v1/search?categories=solar_system&search=Jita+4-4&&flag");
    assert_eq!(path, "/v1/search");
    assert_eq!(q.get("categories").map(String::as_str), Some("solar_system"));
    assert_eq!(q.get("search").map(String::as_str), Some("Jita 4-4"));
    assert_eq!(q.get("flag").map(String::as_str), Some(""));

    for (al, ql, expected) in [
        (None, None, "en"),
        (Some("de"), None, "de"),
        (Some("es"), None, "es"),
        (Some("fr"), None, "fr"),
        (Some("ja"), None, "ja"),
        (Some("ko"), None, "ko"),
        (Some("ru"), None, "ru"),
        (Some("zh"), None, "zh"),
        (Some("en"), Some("de"), "de"),
        (Some("pt-BR"), None, "en"),
    ] {
        assert_eq!(negotiate_esi_language(al, ql), expected);
    }

    // extract_bearer_token: header Bearer / bearer / invalid scheme / query fallback
    let mut req = EsiHttpRequest::new("GET", "/status")
        .with_header("Authorization", "  bearer tok-123  ");
    assert_eq!(
        extract_bearer_token(&req, &HashMap::new()).as_deref(),
        Some("tok-123")
    );
    req.headers
        .insert("Authorization".to_string(), "Basic abc".to_string());
    let mut q_tok = HashMap::new();
    q_tok.insert("token".to_string(), "query-tok".to_string());
    // Invalid Authorization header must NOT fall back to query token
    assert_eq!(extract_bearer_token(&req, &q_tok), None);
    let req_no_hdr = EsiHttpRequest::new("GET", "/status");
    assert_eq!(
        extract_bearer_token(&req_no_hdr, &q_tok).as_deref(),
        Some("query-tok")
    );

    // required_corp_roles_for_route coverage across all role arms
    use eve_db::corp_roles::*;
    assert_eq!(
        required_corp_roles_for_route("GET", "/corporations/{corporation_id}/structures"),
        ROLE_STATION_MANAGER
    );
    assert_eq!(
        required_corp_roles_for_route("GET", "/corporation/{corporation_id}/mining/observers"),
        ROLE_ACCOUNTANT
    );
    assert_eq!(
        required_corp_roles_for_route("GET", "/corporations/{corporation_id}/orders"),
        ROLE_ACCOUNTANT | ROLE_TRADER
    );
    assert_eq!(
        required_corp_roles_for_route("GET", "/corporations/{corporation_id}/industry/jobs"),
        ROLE_FACTORY_MANAGER
    );
    assert_eq!(
        required_corp_roles_for_route("GET", "/corporations/{corporation_id}/wallets"),
        ROLE_ACCOUNTANT | ROLE_JUNIOR_ACCOUNTANT
    );
    assert_eq!(
        required_corp_roles_for_route("GET", "/corporations/{corporation_id}/roles"),
        ROLE_PERSONNEL_MANAGER
    );
    assert_eq!(
        required_corp_roles_for_route("GET", "/corporations/{corporation_id}/roles/history"),
        ROLE_DIRECTOR
    );
    assert_eq!(
        required_corp_roles_for_route("GET", "/corporations/{corporation_id}/contracts"),
        0
    );
    assert_eq!(
        required_corp_roles_for_route("GET", "/corporations/{corporation_id}/starbases"),
        ROLE_DIRECTOR
    );
}

#[test]
fn error_limit_window_pruning_and_420_exhaustion() {
    let mut map = HashMap::new();
    for i in 0..MAX_ERROR_LIMIT_CLIENTS {
        map.insert(
            format!("10.0.0.{i}"),
            ErrorLimitWindow {
                window_start_unix: 1000 + (i as u64),
                errors_used: 1,
            },
        );
    }
    // All windows active -> evicts oldest (window_start_unix == 1000, key "10.0.0.0")
    EsiRouter::prune_error_windows_if_full(&mut map, 1010, 60);
    assert_eq!(map.len(), MAX_ERROR_LIMIT_CLIENTS - 1);
    assert!(!map.contains_key("10.0.0.0"));

    // Custom error limit = 2 -> 2 bad requests yield 404, 3rd yields 420
    let (ctx, _token, _char_id, _corp_id, _planet_id) = seed_full_esi_context();
    let router = EsiRouter::with_error_limit(ctx, 2, 60);
    let mut req = EsiHttpRequest::new("GET", "/nonexistent/route");
    req.peer_ip = "   ".to_string(); // tests default "127.0.0.1" client_bucket_key
    assert_eq!(EsiRouter::client_bucket_key(&req), "127.0.0.1");

    let r1 = router.handle_request(&req);
    assert_eq!(r1.status_code, 404);
    assert_eq!(r1.header("x-esi-error-limit-remain"), Some("1"));

    let r2 = router.handle_request(&req);
    assert_eq!(r2.status_code, 404);
    assert_eq!(r2.header("x-esi-error-limit-remain"), Some("0"));

    let r3 = router.handle_request(&req);
    assert_eq!(r3.status_code, 420);
}

#[test]
fn handle_request_meta_endpoints_cdn_portrait_auth_and_conditional_etag() {
    let (ctx, token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let router = EsiRouter::new(Arc::clone(&ctx));

    // Unsupported compatibility date -> 400; invalid datasource -> 400; invalid X-Tenant -> 400
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", "/status")
                    .with_header("X-Compatibility-Date", "1999-01-01")
            )
            .status_code,
        400
    );
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("GET", "/status?datasource=chaos"))
            .status_code,
        400
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", "/status").with_header("X-Tenant", "invalid")
            )
            .status_code,
        400
    );

    // Meta / swagger / versions / status.json / CDN endpoints
    let mgr = &ctx.quasar_gateway.progression.achievements;
    let cdn_ach = format!(
        "/cdn/achievements/v24.1.{}/{}.json",
        mgr.sde_achievement_count, mgr.sde_achievement_hash
    );
    for path in [
        "/swagger.json",
        "/openapi.json",
        "/meta",
        "/meta/compatibility-dates",
        "/meta/changelog",
        "/versions",
        "/status.json",
        cdn_ach.as_str(),
    ] {
        let res = router.handle_request(&EsiHttpRequest::new("GET", path));
        assert_eq!(res.status_code, 200, "failed on {path}");
    }

    // /verify and /oauth/verify (missing token -> 401; valid token -> 200)
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("GET", "/verify"))
            .status_code,
        401
    );
    let ver =
        router.handle_request(&EsiHttpRequest::new("GET", "/oauth/verify").with_bearer_token(&token));
    assert_eq!(ver.status_code, 200);
    assert_eq!(ver.json().unwrap()["CharacterID"], char_id);

    // Portrait binary image route: hex-encoded JPEG bytes vs 404 when unset
    ctx.db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime.portraits.insert(
            format!("{char_id}:64"),
            "ffd8ffe000104a464946".to_string(),
        );
    });
    let img_ok = router.handle_request(&EsiHttpRequest::new(
        "GET",
        &format!("/images/characters/{char_id}/portrait?size=64"),
    ));
    assert_eq!(img_ok.status_code, 200);
    assert_eq!(img_ok.header("content-type"), Some("image/jpeg"));
    assert_eq!(&img_ok.body[..2], &[0xFF, 0xD8]);

    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new(
                "GET",
                "/images/characters/99999999/portrait"
            ))
            .status_code,
        404
    );

    // 405 Method Not Allowed on valid path with wrong method
    let m405 = router.handle_request(&EsiHttpRequest::new("POST", "/status"));
    assert_eq!(m405.status_code, 405);

    // Character mismatch on protected character route -> 403
    let wrong_char = router.handle_request(
        &EsiHttpRequest::new("GET", "/characters/90000002/wallet").with_bearer_token(&token),
    );
    assert_eq!(wrong_char.status_code, 403);

    // Non-member corporation route -> 403
    let wrong_corp = router.handle_request(
        &EsiHttpRequest::new("GET", &format!("/corporations/{}/wallets", corp_id + 999))
            .with_bearer_token(&token),
    );
    assert_eq!(wrong_corp.status_code, 403);

    // Conditional ETag -> 304 Not Modified with empty body
    let st_200 = router.handle_request(&EsiHttpRequest::new("GET", "/status"));
    assert_eq!(st_200.status_code, 200);
    let etag = st_200.header("etag").unwrap().to_string();
    let st_304 = router.handle_request(
        &EsiHttpRequest::new("GET", "/status").with_header("If-None-Match", &etag),
    );
    assert_eq!(st_304.status_code, 304);
    assert!(st_304.body.is_empty());
}
