//! Edge-case and branch-coverage unit tests for `esi::router`.

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;
use eve_db::corp_roles::*;

fn issue_char_token(ctx: &ServerContext, char_id: i64, scopes: &[&str], now: u64) -> String {
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            char_id,
            "evomu-cli",
            "http://localhost/callback",
            scopes.iter().map(|s| (*s).to_string()).collect(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "S256",
            now,
        )
        .unwrap();
    ctx.db
        .exchange_pkce_code(
            &code,
            "evomu-cli",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            now,
        )
        .unwrap()
        .access_token
}

#[test]
fn error_window_reset_and_ttl_pruning_cover_rollover_branches() {
    let (ctx, _token, _char_id, _corp_id, _planet_id) = seed_full_esi_context();
    let router = EsiRouter::with_error_limit(Arc::clone(&ctx), 2, 10);

    // Exhaust error limit
    let req = EsiHttpRequest::new("GET", "/no-such-route").with_header("X-Forwarded-For", "1.2.3.4");
    assert_eq!(router.handle_request(&req).status_code, 404);
    assert_eq!(router.handle_request(&req).status_code, 404);
    assert_eq!(router.handle_request(&req).status_code, 420);

    // Advance clock past window -> window resets!
    ctx.advance_secs(15);
    let after_reset = router.handle_request(&req);
    assert_eq!(after_reset.status_code, 404);
    assert_eq!(after_reset.header("x-esi-error-limit-remain"), Some("1"));

    // Prune expired windows when full
    let mut map = HashMap::new();
    for i in 0..MAX_ERROR_LIMIT_CLIENTS {
        map.insert(
            format!("10.1.0.{i}"),
            ErrorLimitWindow {
                window_start_unix: if i % 2 == 0 { 100 } else { 1000 },
                errors_used: 1,
            },
        );
    }
    EsiRouter::prune_error_windows_if_full(&mut map, 1010, 60);
    assert_eq!(map.len(), MAX_ERROR_LIMIT_CLIENTS / 2);
}

#[test]
fn handle_request_covers_auth_scopes_roles_alliance_cdn_portrait_lang_and_204() {
    let (ctx, token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let router = EsiRouter::new(Arc::clone(&ctx));
    let now = ctx.now_unix();

    // Compatibility date via query parameter + valid datasource
    let q_compat = router.handle_request(&EsiHttpRequest::new(
        "GET",
        "/status?compatibility_date=2020-01-01&datasource=Tranquility",
    ));
    assert_eq!(q_compat.status_code, 200);

    // /verify: invalid token -> 401; character_id == 0 ("User" token) -> 200
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", "/verify").with_bearer_token("bad-token-xyz"),
            )
            .status_code,
        401
    );
    let user_tok = issue_char_token(&ctx, 0, &[], now);
    let ver_user = router
        .handle_request(&EsiHttpRequest::new("GET", "/verify").with_bearer_token(&user_tok));
    assert_eq!(ver_user.status_code, 200);
    assert_eq!(ver_user.json().unwrap()["TokenType"], "User");

    // Portrait branches: char exists without blob -> 404; non-hex raw blob -> 200
    let char2_id = 90_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime.portraits.remove(&format!("{char2_id}:64"));
        snap.cluster_and_runtime.portraits.remove(&format!("{char2_id}"));
        snap.cluster_and_runtime.portraits.insert(format!("{char_id}"), "raw-jpeg-bytes".to_string());
    });
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new(
                "GET",
                &format!("/images/characters/{char2_id}/portrait?size=64"),
            ))
            .status_code,
        404
    );
    let raw_img = router.handle_request(&EsiHttpRequest::new(
        "GET",
        &format!("/images/characters/{char_id}/portrait?size=128"),
    ));
    assert_eq!(raw_img.status_code, 200);
    assert_eq!(raw_img.body, b"raw-jpeg-bytes");

    // CDN achievements 404 + CDN character title definitions 200 & 404
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new(
                "GET",
                "/cdn/achievements/v24.1.999999/deadbeef.json",
            ))
            .status_code,
        404
    );
    let title_ver = {
        let skinr = crate::services::quasar_cosmetics_skinr::SkinrCosmeticsState::load(
            Arc::clone(&ctx.db),
            Arc::clone(&ctx.sde),
        );
        let d = skinr.data.read().unwrap();
        d.title_def_version
    };
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new(
                "GET",
                &format!(
                    "/cdn/cosmetics/character/title/definitions/v{}.{}.{}/defs.json",
                    title_ver[0], title_ver[1], title_ver[2]
                ),
            ))
            .status_code,
        200
    );
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new(
                "GET",
                "/cdn/cosmetics/character/title/definitions/v99.99.99/defs.json",
            ))
            .status_code,
        404
    );

    // Page validation errors
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("GET", "/universe/types?page=0"))
            .status_code,
        400
    );
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("GET", "/universe/types?page=abc"))
            .status_code,
        400
    );

    // Authenticated route errors: missing token (401), bad token (401), char_id==0 (403), missing scope (403)
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new(
                "GET",
                &format!("/characters/{char_id}/wallet"),
            ))
            .status_code,
        401
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", &format!("/characters/{char_id}/wallet"))
                    .with_bearer_token("invalid-tok"),
            )
            .status_code,
        401
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", &format!("/characters/{char_id}/wallet"))
                    .with_bearer_token(&user_tok),
            )
            .status_code,
        403
    );
    let no_scope_tok = issue_char_token(&ctx, char_id, &["esi-skills.read_skills.v1"], now);
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", &format!("/characters/{char_id}/wallet"))
                    .with_bearer_token(&no_scope_tok),
            )
            .status_code,
        403
    );

    // Non-integer character_id / corporation_id in path -> 400
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", "/characters/notanint/wallet")
                    .with_bearer_token(&token),
            )
            .status_code,
        400
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", "/corporations/notanint/wallets")
                    .with_bearer_token(&token),
            )
            .status_code,
        400
    );

    // Missing required query parameter -> 400 (/characters/{cid}/search requires categories & search)
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", &format!("/characters/{char_id}/search"))
                    .with_bearer_token(&token),
            )
            .status_code,
        400
    );

    // Language negotiation + Content-Language header on /universe/types/{type_id}
    let lang_res = router.handle_request(
        &EsiHttpRequest::new("GET", "/universe/types/587")
            .with_header("Accept-Language", "de"),
    );
    assert_eq!(lang_res.status_code, 200);
    assert_eq!(lang_res.header("content-language"), Some("de"));

    // 204 No Content response has no Content-Type header (PUT /characters/{cid}/calendar/{eid})
    ctx.db.with_mut_snapshot(|snap| {
        snap.calendar_events.insert(
            777,
            eve_db::CalendarEventRecord {
                event_id: 777,
                owner_id: char_id,
                creator_id: char_id,
                event_date_time: 1_700_000_000,
                duration: 30,
                importance: 0,
                title: "Event".to_string(),
                description: "Desc".to_string(),
                scope: "Personal".to_string(),
                responses: HashMap::new(),
                is_deleted: false,
            },
        );
    });
    let mut put_req = EsiHttpRequest::new(
        "PUT",
        &format!("/characters/{char_id}/calendar/777"),
    )
    .with_bearer_token(&token);
    put_req.body = br#"{"response":"accepted"}"#.to_vec();
    let put_res = router.handle_request(&put_req);
    assert_eq!(put_res.status_code, 204);
    assert_eq!(put_res.header("content-type"), None);

    // 5xx response does NOT count against error limit: pick a spec operation that returns 501
    let before2 = router.handle_request(&EsiHttpRequest::new("GET", "/status"));
    let rem2: u32 = before2
        .header("x-esi-error-limit-remain")
        .unwrap()
        .parse()
        .unwrap();
    if let Some(red_op) = OpenApiSpec::pinned().operations.iter().find(|op| {
        !crate::esi::sample_routes::is_operation_implemented(&op.method, &op.path_pattern)
            && !op.path_pattern.contains('{')
            && op.parameters.iter().all(|p| !p.required)
    }) {
        let res_501 = router.handle_request(
            &EsiHttpRequest::new(&red_op.method, &red_op.path_pattern).with_bearer_token(&token),
        );
        if res_501.status_code >= 500 {
            assert_eq!(
                res_501
                    .header("x-esi-error-limit-remain")
                    .unwrap()
                    .parse::<u32>()
                    .unwrap(),
                rem2
            );
        }
    }

    // Alliance guard: non-member char -> 403 on /alliances/{alliance_id}/contacts
    let other_alliance = 99_000_555_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            other_alliance,
            eve_db::AllianceRecord {
                alliance_id: other_alliance,
                name: "Enemy Alliance".to_string(),
                short_name: "ENMY".to_string(),
                executor_corp_id: 98_999_999,
                member_corp_ids: [98_999_999].into_iter().collect(),
            },
        );
    });
    let alli_forb = router.handle_request(
        &EsiHttpRequest::new("GET", &format!("/alliances/{other_alliance}/contacts"))
            .with_bearer_token(&token),
    );
    assert_eq!(alli_forb.status_code, 403);

    // Corp roles route with grantable role (not CEO, not active role)
    let c3 = 90_000_003_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch3 = snap.characters[&char_id].clone();
        ch3.character_id = c3;
        ch3.corporation_id = corp_id;
        snap.characters.insert(c3, ch3);
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .members
            .insert(c3);
        snap.corp_role_assignments.insert(
            (corp_id, c3),
            eve_db::CorpMemberRoleAssignment {
                character_id: c3,
                roles: 0,
                grantable_roles: ROLE_PERSONNEL_MANAGER,
                roles_at_hq: 0,
                grantable_roles_at_hq: 0,
                roles_at_base: 0,
                grantable_roles_at_base: 0,
                roles_at_other: 0,
                grantable_roles_at_other: 0,
            },
        );
    });
    let tok_c3 = issue_char_token(
        &ctx,
        c3,
        &["esi-corporations.read_corporation_membership.v1"],
        now,
    );
    let roles_ok = router.handle_request(
        &EsiHttpRequest::new("GET", &format!("/corporations/{corp_id}/roles"))
            .with_bearer_token(&tok_c3),
    );
    assert_eq!(roles_ok.status_code, 200);
}

#[test]
fn effective_esi_corp_role_breakdown_covers_governance_titles_and_assignments() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let now = 1_700_000_000_u64;
    let c_test = 90_000_050_i64;

    ctx.db.with_mut_snapshot(|snap| {
        // Unknown corp returns default
        let empty = effective_esi_corp_role_breakdown(snap, 99_999_999, char_id, now);
        assert!(!empty.is_ceo);
        assert_eq!(empty.any_active_roles(), 0);
        assert_eq!(empty.any_grantable_roles(), 0);

        snap.corp_role_assignments.insert(
            (corp_id, c_test),
            eve_db::CorpMemberRoleAssignment {
                character_id: c_test,
                roles: ROLE_ACCOUNTANT,
                roles_at_hq: ROLE_TRADER,
                roles_at_base: ROLE_FACTORY_MANAGER,
                roles_at_other: ROLE_STATION_MANAGER,
                grantable_roles: ROLE_PERSONNEL_MANAGER,
                grantable_roles_at_hq: ROLE_DIRECTOR,
                grantable_roles_at_base: ROLE_JUNIOR_ACCOUNTANT,
                grantable_roles_at_other: ROLE_CHAT_MANAGER,
            },
        );
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.pending_role_grants
            .insert(c_test, (ROLE_JUNIOR_ACCOUNTANT, now - 10)); // matured
        gov.pending_title_role_grants
            .insert(c_test, (ROLE_TRADER, now + 1000)); // unmatured -> masked out!
        gov.titles.insert(1, ("Officer".to_string(), ROLE_PERSONNEL_MANAGER));
        gov.member_titles.insert(c_test, 1);

        snap.corp_titles.insert(
            corp_id,
            vec![eve_db::CorpTitleEntry {
                title_id: 2,
                name: "Sec".to_string(),
                roles: vec!["securityOfficer".to_string()],
                grantable_roles: vec!["personnelManager".to_string()],
                roles_at_hq: vec!["accountant".to_string()],
                grantable_roles_at_hq: vec!["trader".to_string()],
                roles_at_base: vec!["factoryManager".to_string()],
                grantable_roles_at_base: vec!["stationManager".to_string()],
                roles_at_other: vec!["juniorAccountant".to_string()],
                grantable_roles_at_other: vec!["director".to_string()],
            }],
        );
        snap.corp_member_titles.insert((corp_id, c_test), vec![2]);

        let eff = effective_esi_corp_role_breakdown(snap, corp_id, c_test, now);
        assert!(!eff.is_ceo);
        assert_ne!(eff.roles & ROLE_ACCOUNTANT, 0);
        assert_ne!(eff.roles & ROLE_JUNIOR_ACCOUNTANT, 0);
        assert_ne!(eff.roles & ROLE_PERSONNEL_MANAGER, 0);
        // Unmatured ROLE_TRADER must be masked out of roles_at_hq
        assert_eq!(eff.roles_at_hq & ROLE_TRADER, 0);
        assert_ne!(eff.unmatured_mask & ROLE_TRADER, 0);
        assert_ne!(eff.any_grantable_roles(), 0);
    });
}
