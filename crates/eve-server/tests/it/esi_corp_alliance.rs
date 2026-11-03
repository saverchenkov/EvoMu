//! `ESI-5` integration tests for `Alliance` and `Corporation` OpenAPI endpoints and
//! companion `feat:esi.alliance.*` / `feat:esi.corporation.*` parity rows backed by
//! `WorldRepository` (`WorldStateSnapshot`) and `SdeRegistry`.

#![forbid(unsafe_code)]

use eve_db::{
    AllianceRecord, CharacterLoginStats, CorporationRecord, InventoryFlag, InventoryItemRecord,
    IskCents,
};
use eve_server::esi::sample_routes::format_iso8601_utc;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

pub(crate) fn issue_token_with_scopes(ctx: &ServerContext, character_id: i64, scopes: Vec<String>) -> String {
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&character_id) {
            ch.user_id = 1;
        }
        Ok(())
    });
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            character_id,
            "evomu-esi5-corp-alliance-test",
            "https://localhost/callback",
            scopes,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-esi5-corp-alliance-test", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

pub(crate) fn check_cached_get(
    router: &EsiRouter,
    spec: &OpenApiSpec,
    req: &EsiHttpRequest,
    expected_status: u16,
    expect_paginated: bool,
) -> Value {
    let resp = router.handle_request(req);
    let raw_path = req.raw_target.split('?').next().unwrap_or(&req.raw_target);
    assert_eq!(
        resp.status_code,
        expected_status,
        "{} {} -> {} ({})",
        req.method,
        req.raw_target,
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
    spec.validate_response(
        &req.method,
        raw_path,
        resp.status_code,
        &resp.headers,
        &resp.body,
    )
    .unwrap_or_else(|e| {
        panic!(
            "OpenAPI validation failed for {} {}: {e}",
            req.method, req.raw_target
        )
    });
    let body = resp.json().expect("valid JSON response body");
    if expected_status == 200 {
        let etag = resp.header("etag").expect("ETag header on 200 response");
        let cc = resp
            .header("cache-control")
            .expect("Cache-Control header on 200 response");
        assert!(
            cc.contains("max-age="),
            "Cache-Control must include max-age: {cc}"
        );
        let expires = resp
            .header("expires")
            .expect("Expires header on 200 response");
        assert!(
            expires.ends_with("GMT"),
            "Expires must be HTTP-date in GMT: {expires}"
        );
        let last_mod = resp
            .header("last-modified")
            .expect("Last-Modified header on 200 response");
        assert!(
            last_mod.ends_with("GMT"),
            "Last-Modified must be HTTP-date in GMT: {last_mod}"
        );
        if expect_paginated {
            assert_eq!(
                resp.header("x-pages"),
                Some("1"),
                "Paginated route {} must include X-Pages header",
                req.raw_target
            );
        }
        let mut req304 = req.clone();
        req304
            .headers
            .insert("If-None-Match".to_string(), etag.to_string());
        let resp304 = router.handle_request(&req304);
        assert_eq!(
            resp304.status_code, 304,
            "Conditional GET with matching If-None-Match must return 304 for {}",
            req.raw_target
        );
        assert!(
            resp304.body.is_empty(),
            "304 Not Modified response body must be empty"
        );
    }
    body
}

#[tokio::test]
async fn test_esi5_alliance_and_corporation_public_identity() {
    // parity: esi:GET /alliances/{alliance_id}/corporations, esi:GET /alliances/{alliance_id}/icons
    // parity: esi:GET /corporations/{corporation_id}, esi:GET /corporations/{corporation_id}/icons
    // parity: feat:esi.alliance.list, feat:esi.alliance.corporations, feat:esi.alliance.icons
    // parity: feat:esi.corporation.public, feat:esi.corporation.npccorps, feat:esi.corporation.icons
    eve_testkit::attest(&[
        "esi:GET /alliances/{alliance_id}/corporations",
        "esi:GET /alliances/{alliance_id}/icons",
        "esi:GET /corporations/{corporation_id}",
        "esi:GET /corporations/{corporation_id}/icons",
        "feat:esi.alliance.list",
        "feat:esi.alliance.corporations",
        "feat:esi.alliance.icons",
        "feat:esi.corporation.public",
        "feat:esi.corporation.npccorps",
        "feat:esi.corporation.icons",
    ]);

    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let wingman_id = 90_000_002_i64;
    let exec_corp_id = 98_500_001_i64;
    let member_corp_id = 98_500_002_i64;
    let alliance_id = 99_500_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            exec_corp_id,
            CorporationRecord {
                corporation_id: exec_corp_id,
                name: "Vanguard Sovereign Logistics".to_string(),
                ticker: "VSLG".to_string(),
                ceo_id,
                alliance_id: Some(alliance_id),
                tax_rate: 0.065,
                master_wallet: IskCents::from_isk(100_000_000),
                members: HashSet::from([ceo_id, wingman_id]),
                hq_station_id: Some(60_003_760),
                description: "Sovereign industrial backbone".to_string(),
                url: "https://vslg.evomu.test".to_string(),
                ..Default::default()
            },
        );
        snap.corporations.insert(
            member_corp_id,
            CorporationRecord {
                corporation_id: member_corp_id,
                name: "Vanguard Mining Wing".to_string(),
                ticker: "VMWG".to_string(),
                ceo_id: wingman_id,
                alliance_id: Some(alliance_id),
                tax_rate: 0.03,
                master_wallet: IskCents::from_isk(25_000_000),
                members: HashSet::from([wingman_id]),
                hq_station_id: Some(60_003_760),
                ..Default::default()
            },
        );
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "Vanguard Sovereign Concordat".to_string(),
                short_name: "VSCON".to_string(),
                executor_corp_id: exec_corp_id,
                member_corp_ids: HashSet::from([exec_corp_id]),
            },
        );
    });

    // 1. GET /alliances (`feat:esi.alliance.list`)
    let alliances_list = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/alliances/"),
        200,
        false,
    );
    let al_ids = alliances_list.as_array().expect("alliances array");
    assert!(al_ids.contains(&json!(alliance_id)));

    // GET /alliances/{alliance_id} without stored alliance_lifecycle_meta returns 404 (no fabricated date_founded/creator_id)
    let al_detail_resp =
        router.handle_request(&EsiHttpRequest::new("GET", format!("/v1/alliances/{alliance_id}/")));
    assert_eq!(al_detail_resp.status_code, 404);

    // 2. GET /alliances/{alliance_id}/corporations (`esi:GET /alliances/{alliance_id}/corporations`, `feat:esi.alliance.corporations`)
    let al_corps_initial = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/alliances/{alliance_id}/corporations/")),
        200,
        false,
    );
    assert_eq!(al_corps_initial, json!([exec_corp_id]));

    // Mutate live alliance membership and verify ESI immediately reflects the new member corporation
    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances
            .get_mut(&alliance_id)
            .unwrap()
            .member_corp_ids
            .insert(member_corp_id);
    });
    let al_corps_updated = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/alliances/{alliance_id}/corporations/")),
        200,
        false,
    );
    assert_eq!(al_corps_updated, json!([exec_corp_id, member_corp_id]));
    assert_eq!(
        check_cached_get(
            &router,
            &spec,
            &EsiHttpRequest::new("GET", "/v1/alliances/999999999/corporations/"),
            404,
            false,
        )["status"],
        json!(404)
    );
    assert_eq!(
        check_cached_get(
            &router,
            &spec,
            &EsiHttpRequest::new("GET", "/v1/alliances/-5/corporations/"),
            400,
            false,
        )["status"],
        json!(400)
    );

    // 3. GET /alliances/{alliance_id}/icons (`esi:GET /alliances/{alliance_id}/icons`, `feat:esi.alliance.icons`)
    let al_icons = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v2/alliances/{alliance_id}/icons/")),
        200,
        false,
    );
    assert_eq!(
        al_icons["px64x64"],
        json!(format!(
            "https://images.evetech.net/alliances/{alliance_id}/logo?tenant=tranquility&size=64"
        ))
    );
    assert_eq!(
        al_icons["px128x128"],
        json!(format!(
            "https://images.evetech.net/alliances/{alliance_id}/logo?tenant=tranquility&size=128"
        ))
    );
    assert_eq!(
        check_cached_get(
            &router,
            &spec,
            &EsiHttpRequest::new("GET", "/v2/alliances/999999999/icons/"),
            404,
            false,
        )["status"],
        json!(404)
    );

    // 4. GET /corporations/npccorps (`feat:esi.corporation.npccorps`)
    let npc_corps = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/corporations/npccorps/"),
        200,
        false,
    );
    let npc_arr = npc_corps.as_array().expect("npccorps array");
    assert_eq!(npc_arr.len(), ctx.sde.npc_corporations.len());
    assert!(npc_arr.contains(&json!(1_000_035))); // Caldari Navy

    // 5. GET /corporations/{corporation_id} (`esi:GET /corporations/{corporation_id}`, `feat:esi.corporation.public`)
    let corp_pub = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v5/corporations/{exec_corp_id}/")),
        200,
        false,
    );
    assert_eq!(corp_pub["name"], json!("Vanguard Sovereign Logistics"));
    assert_eq!(corp_pub["ticker"], json!("VSLG"));
    assert_eq!(corp_pub["ceo_id"], json!(ceo_id));
    assert_eq!(corp_pub["creator_id"], json!(ceo_id));
    assert_eq!(corp_pub["member_count"], json!(2));
    assert_eq!(corp_pub["tax_rate"], json!(0.065));
    assert_eq!(corp_pub["alliance_id"], json!(alliance_id));
    assert_eq!(corp_pub["home_station_id"], json!(60_003_760));
    assert_eq!(
        corp_pub["description"],
        json!("Sovereign industrial backbone")
    );
    assert_eq!(corp_pub["url"], json!("https://vslg.evomu.test"));
    assert!(corp_pub.get("date_founded").is_none());
    assert!(corp_pub.get("shares").is_none());

    // Mutate live corporation tax_rate and members and verify public endpoint reflects the mutation
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.get_mut(&exec_corp_id).unwrap();
        c.tax_rate = 0.08;
        c.members.remove(&wingman_id);
    });
    let corp_pub_updated = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v5/corporations/{exec_corp_id}/")),
        200,
        false,
    );
    assert_eq!(corp_pub_updated["tax_rate"], json!(0.08));
    assert_eq!(corp_pub_updated["member_count"], json!(1));
    assert_eq!(
        check_cached_get(
            &router,
            &spec,
            &EsiHttpRequest::new("GET", "/v5/corporations/999999999/"),
            404,
            false,
        )["status"],
        json!(404)
    );

    // 5b. GET /corporations/{corporation_id} for an SDE NPC corporation (`1000035` Caldari Navy)
    let sde_npc = ctx
        .sde
        .npc_corporations
        .get(&1_000_035)
        .expect("Caldari Navy 1000035 in SDE");
    let expected_npc_ceo = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.corporation_id == 1_000_035)
        .map(|a| a.agent_id)
        .min()
        .unwrap_or(1);
    let npc_corp_pub = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v5/corporations/1000035/"),
        200,
        false,
    );
    assert_eq!(npc_corp_pub["name"], json!(sde_npc.name));
    assert_eq!(npc_corp_pub["ticker"], json!(sde_npc.ticker_name));
    assert_eq!(npc_corp_pub["ceo_id"], json!(expected_npc_ceo));
    assert_eq!(npc_corp_pub["creator_id"], json!(expected_npc_ceo));
    assert_eq!(npc_corp_pub["tax_rate"], json!(sde_npc.tax_rate));
    assert_eq!(npc_corp_pub["faction_id"], json!(sde_npc.faction_id));
    assert_eq!(npc_corp_pub["home_station_id"], json!(sde_npc.station_id));
    assert!(npc_corp_pub.get("date_founded").is_none());
    assert!(npc_corp_pub.get("shares").is_none());

    // 6. GET /corporations/{corporation_id}/icons (`esi:GET /corporations/{corporation_id}/icons`, `feat:esi.corporation.icons`)
    let npc_corp_icons = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/corporations/1000035/icons/"),
        200,
        false,
    );
    assert_eq!(
        npc_corp_icons["px64x64"],
        json!("https://images.evetech.net/corporations/1000035/logo?tenant=tranquility&size=64")
    );
    let corp_icons = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{exec_corp_id}/icons/")),
        200,
        false,
    );
    assert_eq!(
        corp_icons["px64x64"],
        json!(format!(
            "https://images.evetech.net/corporations/{exec_corp_id}/logo?tenant=tranquility&size=64"
        ))
    );
    assert_eq!(
        corp_icons["px128x128"],
        json!(format!(
            "https://images.evetech.net/corporations/{exec_corp_id}/logo?tenant=tranquility&size=128"
        ))
    );
    assert_eq!(
        corp_icons["px256x256"],
        json!(format!(
            "https://images.evetech.net/corporations/{exec_corp_id}/logo?tenant=tranquility&size=256"
        ))
    );
    assert_eq!(
        check_cached_get(
            &router,
            &spec,
            &EsiHttpRequest::new("GET", "/v1/corporations/999999999/icons/"),
            404,
            false,
        )["status"],
        json!(404)
    );
}

#[tokio::test]
async fn test_esi5_corporation_membership_tracking_limit_and_roles() {
    // parity: esi:GET /corporations/{corporation_id}/members
    // parity: esi:GET /corporations/{corporation_id}/membertracking
    // parity: feat:esi.corporation.membertracking
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/members",
        "esi:GET /corporations/{corporation_id}/membertracking",
        "feat:esi.corporation.membertracking",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let wingman_id = 90_000_002_i64;
    let corp_id = 98_500_010_i64;
    let wingman_ship_id = 885_001_i64;
    let stored_logon_unix = 1_710_000_000_u64;
    let stored_logoff_unix = 1_709_996_400_u64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.user_id = 1;
            ch.corporation_id = corp_id;
            ch.station_id = Some(60_003_760);
            ch.solar_system_id = 30_000_142;
        }
        if let Some(w) = snap.characters.get_mut(&wingman_id) {
            w.user_id = 2;
            w.corporation_id = corp_id;
            w.station_id = None;
            w.structure_id = None;
            w.solar_system_id = 30_000_144; // Perimeter
            w.active_ship_id = wingman_ship_id;
        }
        snap.items.insert(
            wingman_ship_id,
            InventoryItemRecord {
                item_id: wingman_ship_id,
                type_id: 587, // Rifter
                owner_id: wingman_id,
                location_id: 30_000_144,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.character_login_stats.insert(
            ceo_id,
            CharacterLoginStats {
                last_login_unix: stored_logon_unix,
                last_logout_unix: stored_logoff_unix,
                logins_count: 12,
            },
        );

        let mut member_roles = HashMap::new();
        member_roles.insert(
            ceo_id,
            eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER,
        );
        member_roles.insert(
            wingman_id,
            eve_db::corp_roles::ROLE_ACCOUNTANT | eve_db::corp_roles::ROLE_FACTORY_MANAGER,
        );

        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Aegis Fleet Directorate".to_string(),
                ticker: "AGFD".to_string(),
                ceo_id,
                tax_rate: 0.05,
                members: HashSet::from([ceo_id, wingman_id]),
                member_roles,
                hq_station_id: Some(60_003_760),
                ..Default::default()
            },
        );
    });

    let token = issue_token_with_scopes(
        &ctx,
        ceo_id,
        vec![
            "esi-corporations.read_corporation_membership.v1".to_string(),
            "esi-corporations.track_members.v1".to_string(),
        ],
    );
    let unscoped_token = issue_token_with_scopes(&ctx, ceo_id, vec![]);
    let non_ceo_token = issue_token_with_scopes(
        &ctx,
        wingman_id,
        vec!["esi-corporations.read_corporation_membership.v1".to_string()],
    );

    // 1. Members (`GET /corporations/{id}/members`) & unbacked `/members/limit` (501)
    let members = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v3/corporations/{corp_id}/members/"))
            .with_bearer_token(&token),
        200,
        false,
    );
    assert_eq!(members, json!([ceo_id, wingman_id]));
    assert_eq!(
        check_cached_get(
            &router,
            &spec,
            &EsiHttpRequest::new("GET", format!("/v3/corporations/{corp_id}/members/"))
                .with_bearer_token(&non_ceo_token),
            403,
            false,
        )["status"],
        json!(403)
    );
    assert_eq!(
        check_cached_get(
            &router,
            &spec,
            &EsiHttpRequest::new("GET", format!("/v3/corporations/{corp_id}/members/"))
                .with_bearer_token(&unscoped_token),
            403,
            false,
        )["status"],
        json!(403)
    );

    let limit_resp = router.handle_request(
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/members/limit/"))
            .with_bearer_token(&token),
    );
    assert_eq!(limit_resp.status_code, 200);

    // 2. Member Tracking (`GET /corporations/{id}/membertracking`, `feat:esi.corporation.membertracking`)
    let tracking = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/membertracking/"))
            .with_bearer_token(&token),
        200,
        false,
    );
    let tr_arr = tracking.as_array().expect("membertracking array");
    assert_eq!(tr_arr.len(), 2);
    assert_eq!(tr_arr[0]["character_id"], json!(ceo_id));
    assert_eq!(tr_arr[0]["location_id"], json!(60_003_760));
    assert_eq!(tr_arr[0]["base_id"], json!(60_003_760));
    assert_eq!(
        tr_arr[0]["logon_date"],
        json!(format_iso8601_utc(stored_logon_unix))
    );
    assert_eq!(
        tr_arr[0]["logoff_date"],
        json!(format_iso8601_utc(stored_logoff_unix))
    );
    assert_eq!(tr_arr[1]["character_id"], json!(wingman_id));
    assert_eq!(tr_arr[1]["location_id"], json!(30_000_144));
    assert_eq!(tr_arr[1]["ship_type_id"], json!(587));
    assert!(tr_arr[1].get("logon_date").is_none());
    assert!(tr_arr[1].get("logoff_date").is_none());
    assert_eq!(
        check_cached_get(
            &router,
            &spec,
            &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/membertracking/")),
            401,
            false,
        )["status"],
        json!(401)
    );

    // 3. Roles (`GET /corporations/{id}/roles` — untagged until grantable/location role masks are persisted)
    let roles = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/roles/"))
            .with_bearer_token(&token),
        200,
        false,
    );
    let r_arr = roles.as_array().expect("roles array");
    assert_eq!(r_arr.len(), 2);
    assert_eq!(r_arr[0]["character_id"], json!(ceo_id));
    assert_eq!(r_arr[0]["roles"], json!(["Director", "Station_Manager"]));
    assert!(r_arr[0].get("grantable_roles").is_none());
    assert_eq!(r_arr[1]["character_id"], json!(wingman_id));
    assert_eq!(r_arr[1]["roles"], json!(["Accountant", "Factory_Manager"]));
}
