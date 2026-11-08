//! Deep unit tests for ESI HTTP/1.1 wire protocol, OAuth2 Bearer & corporation role guards,
//! compatibility dates, error limits, caching headers, and language negotiation.

use super::{
    handle_esi_http_connection, http_reason_phrase, send_raw_esi_request, EsiHttpRequest,
    EsiRouter, EsiServer,
};
use crate::esi::router::split_path_and_query;
use crate::router::ServerContext;
use eve_db::{corp_roles, CorpMemberRoleAssignment, CorpTitleEntry};
use eve_sde::SdeRegistry;
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn build_test_ctx() -> Arc<ServerContext> {
    let ctx = if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let c = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&c.sde));
        c
    };
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut ch) = snap.characters.get_mut(&90_000_001).cloned() {
            ch.user_id = 1;
            ch.alliance_id = Some(99_000_001);
            let corp_id = ch.corporation_id;
            snap.characters.insert(90_000_001, ch.clone());
            for cid in [90_000_002, 90_000_003, 90_000_004] {
                let mut alt = snap.characters.get(&cid).cloned().unwrap_or_else(|| ch.clone());
                alt.user_id = 1;
                alt.character_id = cid;
                alt.name = format!("Pilot {cid}");
                snap.characters.insert(cid, alt);
            }
            let corp = snap.corporations.entry(corp_id).or_insert_with(|| {
                eve_db::CorporationRecord {
                    corporation_id: corp_id,
                    name: "Demo Corp".into(),
                    ticker: "DEMO".into(),
                    ceo_id: 90_000_001,
                    ..Default::default()
                }
            });
            corp.ceo_id = 90_000_001;
            corp.alliance_id = Some(99_000_001);
            corp.members.insert(90_000_001);
            corp.member_roles
                .insert(90_000_001, corp_roles::ROLE_DIRECTOR);
            let alliance = snap.alliances.entry(99_000_001).or_insert_with(|| {
                eve_db::AllianceRecord {
                    alliance_id: 99_000_001,
                    name: "Demo Alliance".into(),
                    short_name: "DEMOA".into(),
                    executor_corp_id: corp_id,
                    member_corp_ids: std::collections::HashSet::from([corp_id]),
                    ..Default::default()
                }
            });
            alliance.member_corp_ids.insert(corp_id);
        }
        snap.cluster_and_runtime
            .portraits
            .insert("90000001:64".into(), "ffd8ffe0".into());
    });
    Arc::new(ctx)
}

fn mint_token(ctx: &ServerContext, char_id: i64, scopes: Vec<String>) -> String {
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            char_id,
            "evomu-cli",
            "http://localhost/callback",
            scopes,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "S256",
            now,
        )
        .expect("authorize_pkce succeeds");
    ctx.db
        .exchange_pkce_code(
            &code,
            "evomu-cli",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            now,
        )
        .expect("exchange_pkce_code succeeds")
        .access_token
}

fn all_openapi_scopes() -> Vec<String> {
    let scopes: BTreeSet<String> = super::OpenApiSpec::pinned()
        .operations
        .iter()
        .flat_map(|op| op.required_scopes.iter().cloned())
        .collect();
    scopes.into_iter().collect()
}

async fn send_raw_wire_bytes(addr: std::net::SocketAddr, raw: &[u8]) -> String {
    let mut stream = TcpStream::connect(addr).await.expect("connect TCP");
    stream.write_all(raw).await.expect("write raw bytes");
    let _ = stream.shutdown().await;
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).await.expect("read response");
    String::from_utf8_lossy(&resp).into_owned()
}

#[tokio::test]
async fn esi_http_wire_chunked_oversized_malformed_headers_and_caching() {
    let ctx = build_test_ctx();
    let (addr, router, handle) = EsiServer::spawn(ctx, "127.0.0.1:0")
        .await
        .expect("spawn ESI HTTP server");

    // 1. Transfer-Encoding: chunked with chunk extension decoded end-to-end
    let ids_body = b"[\"Jita\"]";
    let chunk_wire = format!(
        "POST /v1/universe/ids/ HTTP/1.1\r\nHost: {addr}\r\nTransfer-Encoding: chunked\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{:x};ext=demo\r\n[\"Jita\"]\r\n0\r\n\r\n",
        ids_body.len()
    );
    let resp_text = send_raw_wire_bytes(addr, chunk_wire.as_bytes()).await;
    assert!(resp_text.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(resp_text.contains("\"systems\""));

    // 2. Invalid hex chunk size -> 400 Bad Request
    let bad_chunk = format!(
        "POST /v1/universe/ids/ HTTP/1.1\r\nHost: {addr}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nZZZZ\r\n"
    );
    let bad_chunk_resp = send_raw_wire_bytes(addr, bad_chunk.as_bytes()).await;
    assert!(bad_chunk_resp.starts_with("HTTP/1.1 400 Bad Request\r\n"));
    assert!(bad_chunk_resp.contains("Invalid HTTP chunk size"));

    // 3. Oversized chunk -> 400 Bad Request
    let huge_chunk = format!(
        "POST /v1/universe/ids/ HTTP/1.1\r\nHost: {addr}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nffffffff\r\n"
    );
    let huge_chunk_resp = send_raw_wire_bytes(addr, huge_chunk.as_bytes()).await;
    assert!(huge_chunk_resp.starts_with("HTTP/1.1 400 Bad Request\r\n"));
    assert!(huge_chunk_resp.contains("exceeds maximum allowed size"));

    // 4. Content-Length exceeding MAX_HTTP_BODY_BYTES -> 413 Payload Too Large
    let too_large = format!(
        "GET /v1/status/ HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 99999999\r\nConnection: close\r\n\r\n"
    );
    let resp_413 = send_raw_wire_bytes(addr, too_large.as_bytes()).await;
    assert!(resp_413.starts_with("HTTP/1.1 413 Payload Too Large\r\n"));

    // 5. Non-numeric Content-Length -> 400 Bad Request
    let bad_cl = format!(
        "GET /v1/status/ HTTP/1.1\r\nHost: {addr}\r\nContent-Length: notanumber\r\nConnection: close\r\n\r\n"
    );
    let resp_bad_cl = send_raw_wire_bytes(addr, bad_cl.as_bytes()).await;
    assert!(resp_bad_cl.starts_with("HTTP/1.1 400 Bad Request\r\n"));

    // 6. Oversized header line (> 8192 bytes) -> 400 Bad Request
    let long_val = "A".repeat(9000);
    let long_hdr = format!(
        "GET /v1/status/ HTTP/1.1\r\nHost: {addr}\r\nX-Huge: {long_val}\r\nConnection: close\r\n\r\n"
    );
    let resp_long_hdr = send_raw_wire_bytes(addr, long_hdr.as_bytes()).await;
    assert!(resp_long_hdr.starts_with("HTTP/1.1 400 Bad Request\r\n"));

    // 7. Excessive header count (> 64 headers) -> 400 Bad Request
    let mut many_hdrs = format!("GET /v1/status/ HTTP/1.1\r\nHost: {addr}\r\n");
    for i in 0..70 {
        many_hdrs.push_str(&format!("X-Extra-{i}: val\r\n"));
    }
    many_hdrs.push_str("Connection: close\r\n\r\n");
    let resp_many_hdrs = send_raw_wire_bytes(addr, many_hdrs.as_bytes()).await;
    assert!(resp_many_hdrs.starts_with("HTTP/1.1 400 Bad Request\r\n"));

    // 8. Conditional ETag / If-None-Match (strong, weak W/, comma-list, wildcard *) -> 304 empty body
    let req_base = EsiHttpRequest::new("GET", "/v1/swagger.json").with_peer_ip("10.1.2.3");
    assert_eq!(req_base.peer_ip, "10.1.2.3");
    let resp_200 = send_raw_esi_request(addr, &req_base).await.expect("GET swagger");
    assert_eq!(resp_200.status_code, 200);
    let etag = resp_200.header("etag").expect("ETag present").to_string();

    for inm in [
        etag.clone(),
        format!("W/{etag}"),
        format!("\"other\", {etag}"),
        "*".to_string(),
    ] {
        let cond_req = EsiHttpRequest::new("GET", "/v1/swagger.json").with_header("If-None-Match", inm);
        let cond_resp = send_raw_esi_request(addr, &cond_req).await.expect("conditional GET");
        assert_eq!(cond_resp.status_code, 304);
        assert!(cond_resp.body.is_empty());
    }

    let miss_req = EsiHttpRequest::new("GET", "/v1/swagger.json")
        .with_header("If-None-Match", "\"0000000000000000\"");
    let miss_resp = send_raw_esi_request(addr, &miss_req).await.expect("ETag mismatch");
    assert_eq!(miss_resp.status_code, 200);
    assert!(!miss_resp.body.is_empty());

    // 9. Early EOF handled cleanly by handle_esi_http_connection
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let eof_addr = listener.local_addr().unwrap();
    let r_clone = Arc::clone(&router);
    let srv_task = tokio::spawn(async move {
        let (s, _) = listener.accept().await.unwrap();
        let _ = handle_esi_http_connection(s, r_clone).await;
    });
    let client = TcpStream::connect(eof_addr).await.unwrap();
    drop(client);
    let _ = srv_task.await;

    // 10. All HTTP reason phrases
    for (code, expected) in [
        (200, "OK"),
        (201, "Created"),
        (204, "No Content"),
        (304, "Not Modified"),
        (400, "Bad Request"),
        (401, "Unauthorized"),
        (403, "Forbidden"),
        (404, "Not Found"),
        (405, "Method Not Allowed"),
        (413, "Payload Too Large"),
        (420, "Error Limited"),
        (422, "Unprocessable Entity"),
        (500, "Internal Server Error"),
        (501, "Not Implemented"),
        (503, "Service Unavailable"),
        (599, "Response"),
    ] {
        assert_eq!(http_reason_phrase(code), expected);
    }

    handle.abort();
}

#[test]
fn esi_router_meta_cdn_verify_compat_date_datasource_and_error_limit() {
    let ctx = build_test_ctx();
    let token = mint_token(&ctx, 90_000_001, vec!["esi-wallet.read_character_wallet.v1".into()]);
    let router = EsiRouter::with_error_limit(Arc::clone(&ctx), 2, 60);

    // 1. Meta & introspection endpoints (/meta, /versions, /status.json, /openapi.json)
    let meta = router.handle_request(&EsiHttpRequest::new("GET", "/meta"));
    assert_eq!(meta.status_code, 200);
    assert!(meta.json().unwrap().get("swagger").is_some());

    let versions = router.handle_request(&EsiHttpRequest::new("GET", "/versions"));
    assert_eq!(versions.status_code, 200);
    assert!(versions.json().unwrap().as_array().unwrap().len() >= 5);

    let status_list = router.handle_request(&EsiHttpRequest::new("GET", "/status.json"));
    assert_eq!(status_list.status_code, 200);
    let entries = status_list.json().unwrap();
    assert!(entries.as_array().unwrap().iter().any(|e| e["status"] == "green"));
    assert!(entries.as_array().unwrap().iter().any(|e| e["status"] == "red"));

    // 2. /verify and /oauth/verify auth + token extraction variants
    let verify_no_auth = router
        .handle_request(&EsiHttpRequest::new("GET", "/verify").with_peer_ip("10.0.1.1"));
    assert_eq!(verify_no_auth.status_code, 401);

    let verify_bad_scheme = router.handle_request(
        &EsiHttpRequest::new("GET", "/verify")
            .with_peer_ip("10.0.1.1")
            .with_header("Authorization", "Basic dXNlcjpwYXNz"),
    );
    assert_eq!(verify_bad_scheme.status_code, 401);

    let verify_bad_tok = router.handle_request(
        &EsiHttpRequest::new("GET", "/oauth/verify")
            .with_peer_ip("10.0.1.2")
            .with_bearer_token("invalid-token-value"),
    );
    assert_eq!(verify_bad_tok.status_code, 401);

    // Lowercase bearer prefix + ?token= query parameter fallback
    let verify_lc = router.handle_request(
        &EsiHttpRequest::new("GET", "/verify")
            .with_header("Authorization", format!("bearer {token}")),
    );
    assert_eq!(verify_lc.status_code, 200);
    let vj = verify_lc.json().unwrap();
    assert_eq!(vj["CharacterID"], 90_000_001);
    assert_eq!(vj["TokenType"], "Character");
    assert_eq!(vj["IntellectualProperty"], "EVE");

    let verify_query =
        router.handle_request(&EsiHttpRequest::new("GET", format!("/oauth/verify?token={token}")));
    assert_eq!(verify_query.status_code, 200);

    // 3. Portrait endpoint branches (hex blob, fallback blob, missing char, missing blob, raw non-hex)
    ctx.db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime.portraits.insert("90000001".into(), "aabbccdd".into());
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            snap.cluster_and_runtime
                .portraits
                .insert(format!("{}:64", ch2.character_id), "RAW_BYTES".into());
        }
    });
    let port_ok =
        router.handle_request(&EsiHttpRequest::new("GET", "/images/characters/90000001/portrait?size=64"));
    assert_eq!(port_ok.status_code, 200);
    assert_eq!(port_ok.header("content-type"), Some("image/jpeg"));
    assert_eq!(port_ok.body, vec![0xff, 0xd8, 0xff, 0xe0]);

    let port_fallback =
        router.handle_request(&EsiHttpRequest::new("GET", "/images/characters/90000001/portrait?size=256"));
    assert_eq!(port_fallback.status_code, 200);
    assert_eq!(port_fallback.body, vec![0xaa, 0xbb, 0xcc, 0xdd]);

    let port_raw =
        router.handle_request(&EsiHttpRequest::new("GET", "/images/characters/90000002/portrait?size=64"));
    assert_eq!(port_raw.status_code, 200);
    assert_eq!(port_raw.body, b"RAW_BYTES");

    let port_404_char = router.handle_request(
        &EsiHttpRequest::new("GET", "/images/characters/999888777/portrait").with_peer_ip("10.0.1.3"),
    );
    assert_eq!(port_404_char.status_code, 404);

    // 4. CDN achievements & title definitions (404 mismatch vs 200 valid)
    let ach_mgr = &ctx.quasar_gateway.progression.achievements;
    let ach_ok_path = format!(
        "/cdn/achievements/v24.1.{}/{}.json",
        ach_mgr.sde_achievement_count, ach_mgr.sde_achievement_hash
    );
    assert_eq!(router.handle_request(&EsiHttpRequest::new("GET", ach_ok_path)).status_code, 200);
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", "/cdn/achievements/v24.1.0/wronghash.json")
                    .with_peer_ip("10.0.1.4")
            )
            .status_code,
        404
    );

    let skinr = crate::services::quasar_cosmetics_skinr::SkinrCosmeticsState::load(
        Arc::clone(&ctx.db),
        Arc::clone(&ctx.sde),
    );
    let ver = skinr.data.read().unwrap().title_def_version;
    let titles_ok_path = format!(
        "/cdn/cosmetics/character/title/definitions/v{}.{}.{}/defs.json",
        ver[0], ver[1], ver[2]
    );
    assert_eq!(router.handle_request(&EsiHttpRequest::new("GET", titles_ok_path)).status_code, 200);
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", "/cdn/cosmetics/character/title/definitions/v0.0.0/defs.json")
                    .with_peer_ip("10.0.1.5")
            )
            .status_code,
        404
    );

    // 5. Compatibility date, datasource, X-Tenant, 405 Method Not Allowed, invalid ?page=0, missing query param
    let bad_compat = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/status/")
            .with_header("X-Compatibility-Date", "1999-01-01")
            .with_peer_ip("10.0.1.6"),
    );
    assert_eq!(bad_compat.status_code, 400);

    let bad_ds = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/status/?datasource=singularity").with_peer_ip("10.0.1.7"),
    );
    assert_eq!(bad_ds.status_code, 400);

    let bad_tenant = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/status/")
            .with_header("X-Tenant", "serenity")
            .with_peer_ip("10.0.1.8"),
    );
    assert_eq!(bad_tenant.status_code, 400);

    let method_405 = router
        .handle_request(&EsiHttpRequest::new("DELETE", "/v1/status/").with_peer_ip("10.0.1.9"));
    assert_eq!(method_405.status_code, 405);

    let page_400 = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/status/?page=0").with_peer_ip("10.0.1.10"),
    );
    assert_eq!(page_400.status_code, 400);

    let missing_query = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/markets/10000002/orders/").with_peer_ip("10.0.1.11"),
    );
    assert_eq!(missing_query.status_code, 400);

    // 6. Per-client ErrorLimitWindow exhaustion -> 420 Error Limited -> window expiry recovery
    let err_ip = "198.51.100.77";
    let e1 = router.handle_request(&EsiHttpRequest::new("GET", "/v1/nonexistent").with_peer_ip(err_ip));
    assert_eq!(e1.status_code, 404);
    assert_eq!(e1.header("x-esi-error-limit-remain"), Some("1"));

    let e2 = router.handle_request(&EsiHttpRequest::new("GET", "/v1/nonexistent").with_peer_ip(err_ip));
    assert_eq!(e2.status_code, 404);
    assert_eq!(e2.header("x-esi-error-limit-remain"), Some("0"));

    // Budget exhausted: even valid /v1/status/ returns 420 Error Limited for err_ip
    let e3_420 = router.handle_request(&EsiHttpRequest::new("GET", "/v1/status/").with_peer_ip(err_ip));
    assert_eq!(e3_420.status_code, 420);
    assert_eq!(e3_420.header("x-esi-error-limit-remain"), Some("0"));

    // Independent client IP still succeeds with full budget
    let clean_resp =
        router.handle_request(&EsiHttpRequest::new("GET", "/v1/status/").with_peer_ip("198.51.100.88"));
    assert_eq!(clean_resp.status_code, 200);
    assert_eq!(clean_resp.header("x-esi-error-limit-remain"), Some("2"));

    // Advance clock past window duration -> budget resets to 2
    ctx.clock().advance_secs(61);
    let recovered = router.handle_request(&EsiHttpRequest::new("GET", "/v1/status/").with_peer_ip(err_ip));
    assert_eq!(recovered.status_code, 200);
    assert_eq!(recovered.header("x-esi-error-limit-remain"), Some("2"));
}

#[test]
fn esi_oauth_scope_character_corp_role_matrix_alliance_and_lang_guards() {
    let ctx = build_test_ctx();
    let scopes = all_openapi_scopes();
    let corp_id = ctx
        .db
        .with_snapshot(|s| s.characters[&90_000_001].corporation_id);

    // Add member_id = 90_000_002 to same corp with ONLY ROLE_FACTORY_MANAGER initially
    let member_id: i64 = 90_000_002;
    let outsider_id: i64 = 90_000_003;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&member_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = None;
        }
        if let Some(ch3) = snap.characters.get_mut(&outsider_id) {
            ch3.corporation_id = 98_999_999;
            ch3.alliance_id = None;
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(member_id);
            corp.member_roles
                .insert(member_id, corp_roles::ROLE_FACTORY_MANAGER);
        }
    });

    let tok_member = mint_token(&ctx, member_id, scopes.clone());
    let tok_outsider = mint_token(&ctx, outsider_id, scopes.clone());
    let tok_no_scope = mint_token(&ctx, 90_000_001, vec![]);
    let router = EsiRouter::with_error_limit(Arc::clone(&ctx), 200, 60);

    // 1. Missing token -> 401; missing scope -> 403; cross-character token -> 403
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("GET", "/v1/characters/90000001/wallet/"))
            .status_code,
        401
    );
    for (tok, expected) in [(&tok_no_scope, 403), (&tok_member, 403)] {
        let resp = router.handle_request(
            &EsiHttpRequest::new("GET", "/v1/characters/90000001/wallet/").with_bearer_token(tok),
        );
        assert_eq!(resp.status_code, expected);
    }

    // 2. Corporation Role Guard Matrix across all role match arms:
    for allowed_subpath in ["facilities", "industry/jobs", "medals"] {
        let resp = router.handle_request(
            &EsiHttpRequest::new(
                "GET",
                format!("/v1/corporations/{corp_id}/{allowed_subpath}/"),
            )
            .with_bearer_token(&tok_member),
        );
        assert_eq!(resp.status_code, 200, "allowed /{allowed_subpath}/");
    }
    for forbidden_subpath in [
        "wallets",
        "orders",
        "structures",
        "roles",
        "roles/history",
        "members",
    ] {
        let resp = router.handle_request(
            &EsiHttpRequest::new(
                "GET",
                format!("/v1/corporations/{corp_id}/{forbidden_subpath}/"),
            )
            .with_bearer_token(&tok_member),
        );
        assert_eq!(
            resp.status_code, 403,
            "expected 403 for member without role on /{forbidden_subpath}/"
        );
    }

    // Outsider in different corp denied even on required_role_mask == 0 corp routes
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/medals/"))
                    .with_bearer_token(&tok_outsider)
            )
            .status_code,
        403
    );

    // Grant ROLE_JUNIOR_ACCOUNTANT + ROLE_TRADER + ROLE_STATION_MANAGER + title grantable roles
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_role_assignments.insert(
            (corp_id, member_id),
            CorpMemberRoleAssignment {
                character_id: member_id,
                roles: corp_roles::ROLE_TRADER,
                grantable_roles: corp_roles::ROLE_PERSONNEL_MANAGER,
                roles_at_hq: corp_roles::ROLE_STATION_MANAGER,
                roles_at_other: corp_roles::ROLE_JUNIOR_ACCOUNTANT,
                ..Default::default()
            },
        );
        snap.corp_titles.insert(
            corp_id,
            vec![CorpTitleEntry {
                title_id: 1,
                name: "Officer".into(),
                grantable_roles: vec!["Personnel_Manager".into()],
                ..Default::default()
            }],
        );
        snap.corp_member_titles.insert((corp_id, member_id), vec![1]);
    });
    for now_allowed in ["orders", "structures", "roles"] {
        let resp = router.handle_request(
            &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/{now_allowed}/"))
                .with_bearer_token(&tok_member),
        );
        assert_eq!(resp.status_code, 200, "role/grantable bitmask for /{now_allowed}/");
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.member_roles
                .insert(member_id, corp_roles::ROLE_JUNIOR_ACCOUNTANT);
        }
    });
    let wallets_resp = router.handle_request(
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/wallets/"))
            .with_bearer_token(&tok_member),
    );
    assert_eq!(wallets_resp.status_code, 200);
    let roles_hist = router.handle_request(
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/roles/history/"))
            .with_bearer_token(&tok_member),
    );
    assert_eq!(roles_hist.status_code, 403);

    // 3. Alliance membership guard
    let alliance_forbidden = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/alliances/99000001/contacts/")
            .with_bearer_token(&tok_outsider),
    );
    assert_eq!(alliance_forbidden.status_code, 403);

    // 4. Language negotiation across all 8 languages + percent decoding with +, %3a, %3A, invalid %ZZ
    let (p, q) = split_path_and_query("/v1/status/?msg=hello+world&colon=%3a%3A&bad=%ZZ&empty=&");
    assert_eq!(p, "/v1/status/");
    assert_eq!(q.get("msg").map(String::as_str), Some("hello world"));
    assert_eq!(q.get("colon").map(String::as_str), Some("::"));
    assert_eq!(q.get("bad").map(String::as_str), Some("%ZZ"));
    assert_eq!(q.get("empty").map(String::as_str), Some(""));

    for lang in ["de", "es", "fr", "ja", "ko", "ru", "zh", "en", "pt"] {
        let r_hdr = router.handle_request(
            &EsiHttpRequest::new("GET", "/v1/universe/types/34/")
                .with_header("Accept-Language", lang),
        );
        assert_eq!(r_hdr.status_code, 200);
        let body_val: Value = r_hdr.json().unwrap();
        assert_eq!(body_val["type_id"], 34);
    }
}
