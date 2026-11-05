//! Unit tests for ESI HTTP/1.1 transport, OpenAPI matcher, OAuth2 scope enforcement, caching, and error limits.

use super::runtime_state::{EsiMailMeta, EsiRuntimeState};
use super::{
    compute_etag, format_http_date, if_none_match_matches,
    send_raw_esi_request, EsiHttpRequest, EsiRouter, EsiServer, OpenApiSpec,
};
use crate::router::ServerContext;
use eve_db::corp_roles;
use eve_sde::SdeRegistry;
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> Arc<ServerContext> {
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
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.alliance_id = Some(99_000_001);
            if let Some(corp) = snap.corporations.get_mut(&ch.corporation_id) {
                corp.ceo_id = 90_000_001;
                corp.alliance_id = Some(99_000_001);
                corp.members.insert(90_000_001);
                corp.member_roles.insert(
                    90_000_001,
                    corp_roles::ROLE_DIRECTOR
                        | corp_roles::ROLE_ACCOUNTANT
                        | corp_roles::ROLE_FACTORY_MANAGER
                        | corp_roles::ROLE_STATION_MANAGER
                        | corp_roles::ROLE_PERSONNEL_MANAGER
                        | corp_roles::ROLE_TRADER,
                );
            }
        }
        snap.service_state_blobs
            .insert("portrait:90000001:64".into(), "ffd8ffe0".into());
    });
    Arc::new(ctx)
}

fn mint_token_with_scopes(ctx: &ServerContext, scopes: Vec<String>) -> String {
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            90_000_001,
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

pub(crate) fn mint_full_scope_token(ctx: &ServerContext) -> String {
    let scopes: BTreeSet<String> = OpenApiSpec::pinned()
        .operations
        .iter()
        .flat_map(|op| op.required_scopes.iter().cloned())
        .collect();
    mint_token_with_scopes(ctx, scopes.into_iter().collect())
}

#[tokio::test]
async fn esi_http_server_tcp_and_wire_serialization() {
    let ctx = test_ctx();
    let token = mint_full_scope_token(&ctx);
    let (addr, _router, handle) = EsiServer::spawn(ctx, "127.0.0.1:0")
        .await
        .expect("spawn ESI server");

    let req = EsiHttpRequest::new("GET", "/v1/status/")
        .with_header("Accept", "application/json");
    let resp = send_raw_esi_request(addr, &req).await.expect("HTTP request succeeds");
    assert_eq!(resp.status_code, 200);
    let val = resp.json().expect("valid JSON");
    assert!(val.get("players").is_some());

    // Direct TCP socket write covering chunked transfer encoding & HTTP/1.1 framing
    let mut client_io = tokio::net::TcpStream::connect(addr).await.unwrap();
    let raw_wire = format!(
        "POST /v2/universe/names/ HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n9\r\n[90000001\r\n1\r\n]\r\n0\r\n\r\n"
    );
    client_io.write_all(raw_wire.as_bytes()).await.unwrap();
    client_io.flush().await.unwrap();
    let mut out = Vec::new();
    let _ = client_io.read_to_end(&mut out).await;
    let out_str = String::from_utf8_lossy(&out);
    assert!(out_str.contains("200 OK"));
    handle.abort();

    let etag = compute_etag(b"hello");
    assert!(if_none_match_matches(&etag, &etag));
    assert!(if_none_match_matches("*", &etag));
    assert!(!if_none_match_matches("\"other\"", &etag));
    assert!(format_http_date(1_700_000_000).contains("GMT"));
}

#[test]
fn esi_meta_auth_scopes_etag_language_and_error_limit_enforcement() {
    let ctx = test_ctx();
    let router = EsiRouter::with_error_limit(Arc::clone(&ctx), 50, 60);
    let full_token = mint_full_scope_token(&ctx);
    let limited_token =
        mint_token_with_scopes(&ctx, vec!["esi-wallet.read_character_wallet.v1".into()]);

    // Meta & introspection routes
    for path in [
        "/swagger.json",
        "/openapi.json",
        "/meta",
        "/versions",
        "/status.json",
        "/v1/meta/compatibility-dates/",
        "/v1/meta/changelog/",
        "/v1/status/",
    ] {
        let r = router.handle_request(&EsiHttpRequest::new("GET", path));
        assert_eq!(r.status_code, 200, "GET {path}");
    }

    // Conditional ETag -> 304 Not Modified
    let first = router.handle_request(&EsiHttpRequest::new("GET", "/v1/versions/"));
    let etag = first.header("etag").expect("ETag present").to_string();
    let cached = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/versions/").with_header("If-None-Match", etag),
    );
    assert_eq!(cached.status_code, 304);
    assert!(cached.body.is_empty());

    // /verify and portrait & CDN endpoints
    assert_eq!(
        router.handle_request(&EsiHttpRequest::new("GET", "/verify/")).status_code,
        401
    );
    let verify_ok = router.handle_request(
        &EsiHttpRequest::new("GET", "/oauth/verify").with_bearer_token(&full_token),
    );
    assert_eq!(verify_ok.status_code, 200);
    assert_eq!(verify_ok.json().unwrap()["CharacterID"], json!(90_000_001));

    let portrait = router.handle_request(&EsiHttpRequest::new(
        "GET",
        "/images/characters/90000001/portrait?size=64",
    ));
    assert_eq!(portrait.status_code, 200);

    let ach_path = format!(
        "/cdn/achievements/v24.1.{}/{}.json",
        ctx.quasar_gateway.progression.achievements.sde_achievement_count,
        ctx.quasar_gateway.progression.achievements.sde_achievement_hash
    );
    assert_eq!(
        router.handle_request(&EsiHttpRequest::new("GET", &ach_path)).status_code,
        200
    );
    let skinr = crate::services::quasar_cosmetics_skinr::SkinrCosmeticsState::load(
        Arc::clone(&ctx.db),
        Arc::clone(&ctx.sde),
    );
    let title_ver = skinr.data.read().unwrap().title_def_version;
    let title_path = format!(
        "/cdn/cosmetics/character/title/definitions/v{}.{}.{}/titles.json",
        title_ver[0], title_ver[1], title_ver[2]
    );
    assert_eq!(
        router.handle_request(&EsiHttpRequest::new("GET", &title_path)).status_code,
        200
    );

    // Language negotiation & invalid parameters
    let de_type = router.handle_request(
        &EsiHttpRequest::new("GET", "/v3/universe/types/587/").with_header("Accept-Language", "de"),
    );
    assert_eq!(de_type.status_code, 200);
    assert_eq!(de_type.header("content-language"), Some("de"));

    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", "/v1/status/")
                    .with_header("X-Compatibility-Date", "1999-01-01")
            )
            .status_code,
        400
    );
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("GET", "/v1/status/?datasource=invalid"))
            .status_code,
        400
    );
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("GET", "/v1/universe/types/?page=0"))
            .status_code,
        400
    );
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("DELETE", "/v1/status/"))
            .status_code,
        405
    );
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("GET", "/v1/does/not/exist"))
            .status_code,
        404
    );

    // Scope check (403) & character ownership mismatch (403)
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", "/v1/characters/90000001/mail/")
                    .with_bearer_token(&limited_token)
            )
            .status_code,
        403
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", "/v1/characters/90000002/wallet/")
                    .with_bearer_token(&limited_token)
            )
            .status_code,
        403
    );

    // Per-IP 420 Error Limited after exceeding budget (from a dedicated client IP)
    let throttled_router = EsiRouter::with_error_limit(Arc::clone(&ctx), 4, 60);
    for _ in 0..4 {
        let r = throttled_router.handle_request(
            &EsiHttpRequest::new("GET", "/v1/nonexistent_endpoint")
                .with_peer_ip("10.99.88.77:1234"),
        );
        assert_eq!(r.status_code, 404);
    }
    let throttled = throttled_router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/status/").with_peer_ip("10.99.88.77:1234"),
    );
    assert_eq!(throttled.status_code, 420);

    // EsiRuntimeState helpers
    let rt = EsiRuntimeState::new();
    let id1 = rt.alloc_id();
    let id2 = rt.alloc_id();
    assert!(id2 > id1);
    assert!(!EsiRuntimeState::default_mail_labels().is_empty());
    rt.mark_mail_deleted(90_000_001, 50);
    assert!(rt.is_mail_deleted(90_000_001, 50));
    rt.set_mail_meta(
        90_000_001,
        51,
        EsiMailMeta {
            is_read: true,
            labels: vec![1, 2],
        },
    );
    assert!(rt.get_mail_meta(90_000_001, 51, false).is_read);
    rt.set_custom_item_name(100_000_001, "My Rifter");
    assert_eq!(
        rt.get_custom_item_name(100_000_001).as_deref(),
        Some("My Rifter")
    );
}
