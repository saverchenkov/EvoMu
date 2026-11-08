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
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = exec_corp_id;
                __corp.name = "Vanguard Sovereign Logistics".to_string();
                __corp.ticker = "VSLG".to_string();
                __corp.ceo_id = ceo_id;
                __corp.alliance_id = Some(alliance_id);
                __corp.tax_rate = 0.065;
                __corp.members = HashSet::from([ceo_id, wingman_id]);
                __corp.hq_station_id = Some(60_003_760);
                __corp.description = "Sovereign industrial backbone".to_string();
                __corp.url = "https://vslg.evomu.test".to_string();
                __corp.set_division_balance(1, IskCents::from_isk(100_000_000));
                __corp },
        );
        snap.corporations.insert(
            member_corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = member_corp_id;
                __corp.name = "Vanguard Mining Wing".to_string();
                __corp.ticker = "VMWG".to_string();
                __corp.ceo_id = wingman_id;
                __corp.alliance_id = Some(alliance_id);
                __corp.tax_rate = 0.03;
                __corp.members = HashSet::from([wingman_id]);
                __corp.hq_station_id = Some(60_003_760);
                __corp.set_division_balance(1, IskCents::from_isk(25_000_000));
                __corp },
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
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Aegis Fleet Directorate".to_string();
                __corp.ticker = "AGFD".to_string();
                __corp.ceo_id = ceo_id;
                __corp.tax_rate = 0.05;
                __corp.members = HashSet::from([ceo_id, wingman_id]);
                __corp.member_roles = member_roles;
                __corp.hq_station_id = Some(60_003_760);
                __corp },
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


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod esi_corp_ext_2 {
    use super::*;

// `ESI-5` integration tests (part 2) for `Corporation` blueprints, divisions, standings,
// structures, facilities, and starbases.


use super::{check_cached_get, issue_token_with_scopes};
use eve_db::corp_roles::{ROLE_DIRECTOR, ROLE_FACTORY_MANAGER, ROLE_STATION_MANAGER};
use eve_db::{
    CorpWalletDivisionRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    StandingTransactionRecord, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::sample_routes::format_iso8601_utc;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

async fn rpc_call(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> PyRep {
    ctx.dispatch_rpc(session, service, None, method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("RPC {service}.{method} failed: {e:?}"))
}

#[tokio::test]
async fn test_esi5_corporation_divisions_blueprints_and_standings() {
    // parity: esi:GET /corporations/{corporation_id}/standings, feat:esi.corporation.standings
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/standings",
        "feat:esi.corporation.standings",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let corp_id = 98_500_020_i64;
    let jita_station = 60_003_760_i64;
    let bpo_id = 885_101_i64;
    let (&sde_agent_id, _) = ctx.sde.agents.iter().next().expect("SDE agent");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.user_id = 1;
            ch.corporation_id = corp_id;
            ch.station_id = Some(jita_station);
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_isk(500_000_000);
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Kaalakiota Industrial Foundry".to_string();
                __corp.ticker = "KAAL".to_string();
                __corp.ceo_id = ceo_id;
                __corp.tax_rate = 0.04;
                __corp.members = HashSet::from([ceo_id]);
                __corp.member_roles = HashMap::from([(
                    ceo_id,
                    ROLE_DIRECTOR | ROLE_FACTORY_MANAGER | ROLE_STATION_MANAGER,
                )]);
                __corp.hq_station_id = Some(jita_station);
                __corp.wallet_divisions = vec![
                    CorpWalletDivisionRecord {
                        division_id: 1,
                        name: "Capital Reserve".to_string(),
                        balance: IskCents::from_isk(500_000_000),
                    },
                    CorpWalletDivisionRecord {
                        division_id: 2,
                        name: "R&D Operations".to_string(),
                        balance: IskCents::from_isk(80_000_000),
                    },
                ];
                let __mw = IskCents::from_isk(100_000_000);
                if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); }
                __corp },
        );

        // Seed unresearched corp BPO in Hangar (CorpSAG1); ME/TE research and BPC copy are driven via live RPCs.
        snap.items.insert(
            bpo_id,
            InventoryItemRecord {
                item_id: bpo_id,
                type_id: 688, // Rifter Blueprint
                owner_id: corp_id,
                location_id: jita_station,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );

        // Corporation standings from faction (500001 Caldari State), NPC corp (1000035 Caldari Navy), and SDE agent
        snap.standing_transactions
            .entry(ceo_id)
            .or_default()
            .extend([
                StandingTransactionRecord {
                    event_id: 1,
                    from_id: sde_agent_id,
                    to_id: corp_id,
                    modification: 2.5,
                    event_type_id: 1,
                },
                StandingTransactionRecord {
                    event_id: 2,
                    from_id: 1_000_035,
                    to_id: corp_id,
                    modification: 4.75,
                    event_type_id: 1,
                },
                StandingTransactionRecord {
                    event_id: 3,
                    from_id: 500_001,
                    to_id: corp_id,
                    modification: 6.1,
                    event_type_id: 1,
                },
            ]);
    });

    let mut sess = SessionState::new(9850);
    sess.user_id = Some(1);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    sess.character_id = Some(ceo_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(jita_station);
    sess.solar_system_id = Some(30_000_142);
    sess.corp_role = ROLE_DIRECTOR | ROLE_FACTORY_MANAGER | ROLE_STATION_MANAGER;

    // Drive ME research (8 runs -> ME 8), TE research (8 runs -> TE 16), and BPC copy (5 runs) via live RPCs.
    for (activity_id, runs) in [(4_i64, 8_i64), (3_i64, 8_i64)] {
        let job_dict = PyRep::Dict(vec![
            (PyRep::String("blueprintID".into()), PyRep::Integer(bpo_id)),
            (PyRep::String("activityID".into()), PyRep::Integer(activity_id)),
            (PyRep::String("runs".into()), PyRep::Integer(runs)),
            (PyRep::String("facilityID".into()), PyRep::Integer(jita_station)),
            (PyRep::String("useCorp".into()), PyRep::Bool(true)),
        ]);
        let job_rep = rpc_call(&ctx, &mut sess, "industryManager", "InstallJob", &[job_dict]).await;
        let job_id = job_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("research jobID");
        let dur = job_rep.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap_or(100_000) as u64;
        ctx.db.clock().advance_secs(dur + 1);
        rpc_call(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(job_id)]).await;
    }

    let copy_dict = PyRep::Dict(vec![
        (PyRep::String("blueprintID".into()), PyRep::Integer(bpo_id)),
        (PyRep::String("activityID".into()), PyRep::Integer(5)),
        (PyRep::String("runs".into()), PyRep::Integer(5)),
        (PyRep::String("facilityID".into()), PyRep::Integer(jita_station)),
        (PyRep::String("useCorp".into()), PyRep::Bool(true)),
    ]);
    let copy_rep = rpc_call(&ctx, &mut sess, "industryManager", "InstallJob", &[copy_dict]).await;
    let copy_job_id = copy_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("copy jobID");
    let copy_dur = copy_rep.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap_or(100_000) as u64;
    ctx.db.clock().advance_secs(copy_dur + 1);
    let copy_done =
        rpc_call(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(copy_job_id)]).await;
    let bpc_id = copy_done
        .dict_get("createdBpcItemID")
        .and_then(|v| v.as_i64())
        .expect("createdBpcItemID");

    let token = issue_token_with_scopes(
        &ctx,
        ceo_id,
        vec![
            "esi-corporations.read_blueprints.v1".to_string(),
            "esi-corporations.read_divisions.v1".to_string(),
            "esi-corporations.read_standings.v1".to_string(),
        ],
    );

    // 1. Blueprints (`GET /corporations/{id}/blueprints`, `feat:esi.corporation.blueprints`)
    let bps = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v2/corporations/{corp_id}/blueprints/"))
            .with_bearer_token(&token),
        200,
        true,
    );
    let bp_arr = bps.as_array().expect("blueprints array");
    assert_eq!(bp_arr.len(), 6);
    let bpo_entry = bp_arr.iter().find(|bp| bp["item_id"] == json!(bpo_id)).expect("BPO entry");
    assert_eq!(bpo_entry["type_id"], json!(688));
    assert_eq!(bpo_entry["location_flag"], json!("CorpSAG1"));
    assert_eq!(bpo_entry["quantity"], json!(-1));
    assert_eq!(bpo_entry["runs"], json!(-1));
    assert_eq!(bpo_entry["material_efficiency"], json!(8));
    assert_eq!(bpo_entry["time_efficiency"], json!(16));
    let bpc_entry = bp_arr.iter().find(|bp| bp["item_id"] == json!(bpc_id)).expect("created BPC entry");
    assert_eq!(bpc_entry["location_flag"], json!("CorpSAG1"));
    assert_eq!(bpc_entry["quantity"], json!(-2));
    assert_eq!(bpc_entry["runs"], json!(5));
    assert_eq!(bpc_entry["material_efficiency"], json!(8));
    assert_eq!(bpc_entry["time_efficiency"], json!(16));
    assert_eq!(
        check_cached_get(
            &router,
            &spec,
            &EsiHttpRequest::new(
                "GET",
                format!("/v2/corporations/{corp_id}/blueprints/?page=2"),
            )
            .with_bearer_token(&token),
            404,
            false,
        )["status"],
        json!(404)
    );

    // 2. Divisions (`GET /corporations/{id}/divisions` — separate hangar_divisions vs wallet_divisions)
    let divs_resp = router.handle_request(
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/divisions/"))
            .with_bearer_token(&token),
    );
    assert_eq!(divs_resp.status_code, 200);

    // 3. Standings (`GET /corporations/{id}/standings`, `feat:esi.corporation.standings`)
    let standings = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/standings/"))
            .with_bearer_token(&token),
        200,
        true,
    );
    let st_arr = standings.as_array().expect("standings array");
    assert_eq!(st_arr.len(), 3);
    assert!(st_arr.iter().any(|s| {
        s["from_id"] == json!(sde_agent_id)
            && s["from_type"] == json!("agent")
            && s["standing"] == json!(2.5)
    }));
    assert!(st_arr.iter().any(|s| {
        s["from_id"] == json!(1_000_035)
            && s["from_type"] == json!("npc_corp")
            && s["standing"] == json!(4.75)
    }));
    assert!(st_arr.iter().any(|s| {
        s["from_id"] == json!(500_001)
            && s["from_type"] == json!("faction")
            && s["standing"] == json!(6.1)
    }));
}

#[tokio::test]
async fn test_esi5_corporation_structures_facilities_and_starbases() {
    // parity: esi:GET /corporations/{corporation_id}/facilities, esi:GET /corporations/{corporation_id}/starbases
    // parity: feat:esi.corporation.facilities, feat:esi.corporation.starbases
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/facilities",
        "esi:GET /corporations/{corporation_id}/starbases",
        "feat:esi.corporation.facilities",
        "feat:esi.corporation.starbases",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let corp_id = 98_500_030_i64;
    let struct_id = 1_035_000_030_001_i64;
    let undamaged_struct_id = 1_035_000_030_002_i64;
    let svc_online_id = 885_201_i64;
    let svc_offline_id = 885_202_i64;
    let starbase_id = 885_203_i64;
    let non_tower_pos_id = 885_204_i64;
    let timer_start_unix = 1_710_000_000_u64;
    let timer_end_unix = 1_710_010_800_u64;
    let next_apply_unix = 1_710_090_000_u64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.user_id = 1;
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Upwell Deep Space Engineering".to_string();
                __corp.ticker = "UDSE".to_string();
                __corp.ceo_id = ceo_id;
                __corp.tax_rate = 0.05;
                __corp.members = HashSet::from([ceo_id]);
                __corp.hq_station_id = Some(60_003_760);
                __corp },
        );

        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_825,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_144,
                name: "Perimeter Raitaru Citadel".to_string(),
                position: [12_000.0, -8_000.0, 44_000.0],
                state: "ArmorReinforced".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 300,
                shield_hp: 0.0,
                armor_hp: 4_500.0,
                hull_hp: 5_000.0,
                reinforced_until_unix: timer_end_unix,
                last_damage_tick: 1,
                last_damage_unix: timer_start_unix,
                self_destruct_at_unix: None,
            },
        );
        snap.structure_vulnerability_windows
            .insert(struct_id, (16 * 3600, next_apply_unix));

        snap.structures.insert(
            undamaged_struct_id,
            UpwellStructureRecord {
                structure_id: undamaged_struct_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_144,
                name: "Perimeter Astrahus Bastion".to_string(),
                position: [-15_000.0, 4_000.0, 12_000.0],
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 5_000.0,
                armor_hp: 5_000.0,
                hull_hp: 5_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.structure_vulnerability_windows
            .insert(undamaged_struct_id, (18 * 3600, next_apply_unix));

        snap.items.insert(
            svc_online_id,
            InventoryItemRecord {
                item_id: svc_online_id,
                type_id: 35_894,
                owner_id: corp_id,
                location_id: struct_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(svc_online_id, true);
        snap.items.insert(
            svc_offline_id,
            InventoryItemRecord {
                item_id: svc_offline_id,
                type_id: 35_892,
                owner_id: corp_id,
                location_id: struct_id,
                flag: InventoryFlag::ServiceSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let token = issue_token_with_scopes(
        &ctx,
        ceo_id,
        vec![
            "esi-corporations.read_facilities.v1".to_string(),
            "esi-corporations.read_starbases.v1".to_string(),
            "esi-corporations.read_structures.v1".to_string(),
        ],
    );

    let facilities = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/facilities/"))
            .with_bearer_token(&token),
        200,
        false,
    );
    assert_eq!(
        facilities,
        json!([{
            "facility_id": struct_id,
            "type_id": 35_825,
            "system_id": 30_000_144
        }])
    );

    let structures = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v4/corporations/{corp_id}/structures/"))
            .with_bearer_token(&token),
        200,
        true,
    );
    let st_arr = structures.as_array().expect("structures array");
    assert_eq!(st_arr.len(), 2);
    let st = &st_arr[0];
    assert_eq!(st["structure_id"], json!(struct_id));
    assert_eq!(st["type_id"], json!(35_825));
    assert_eq!(st["corporation_id"], json!(corp_id));
    assert_eq!(st["system_id"], json!(30_000_144));
    assert_eq!(st["name"], json!("Perimeter Raitaru Citadel"));
    assert_eq!(st["state"], json!("armor_reinforce"));
    assert_eq!(st["reinforce_hour"], json!(16));
    assert_eq!(
        st["next_reinforce_hour"],
        json!(((next_apply_unix / 3600) % 24) as i64)
    );
    assert_eq!(
        st["state_timer_start"],
        json!(format_iso8601_utc(timer_start_unix))
    );
    assert_eq!(
        st["state_timer_end"],
        json!(format_iso8601_utc(timer_end_unix))
    );
    assert_eq!(
        st["next_reinforce_apply"],
        json!(format_iso8601_utc(next_apply_unix))
    );
    assert!(st.get("fuel_expires").is_none());

    let svcs = st["services"].as_array().expect("structure services array");
    assert_eq!(svcs.len(), 2);
    let sde_svc0_name = &ctx.sde.types.get(&35_894).expect("35894 in SDE").name;
    let sde_svc1_name = &ctx.sde.types.get(&35_892).expect("35892 in SDE").name;
    assert_eq!(
        svcs[0],
        json!({ "name": sde_svc0_name, "state": "online" })
    );
    assert_eq!(
        svcs[1],
        json!({ "name": sde_svc1_name, "state": "offline" })
    );

    let st2 = &st_arr[1];
    assert_eq!(st2["structure_id"], json!(undamaged_struct_id));
    assert_eq!(st2["reinforce_hour"], json!(18));
    assert!(st2.get("state_timer_start").is_none());
    assert!(st2.get("state_timer_end").is_none());
    assert!(st2.get("fuel_expires").is_none());

    let sb_empty = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/starbases/"))
            .with_bearer_token(&token),
        200,
        true,
    );
    assert_eq!(sb_empty, json!([]));

    let tower_type = ctx.sde.types.get(&12_235).expect("12235 in SDE");
    assert_eq!(tower_type.category_id, 23);
    assert_eq!(tower_type.group_id, 365);
    let non_tower_pos_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 23 && t.group_id != 365)
        .expect("non-tower category-23 POS module in SDE")
        .type_id;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            starbase_id,
            InventoryItemRecord {
                item_id: starbase_id,
                type_id: 12_235,
                owner_id: corp_id,
                location_id: 30_000_144,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            non_tower_pos_id,
            InventoryItemRecord {
                item_id: non_tower_pos_id,
                type_id: non_tower_pos_type_id,
                owner_id: corp_id,
                location_id: 30_000_144,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let sb_populated = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/starbases/"))
            .with_bearer_token(&token),
        200,
        true,
    );
    assert_eq!(
        sb_populated,
        json!([{
            "starbase_id": starbase_id,
            "type_id": 12_235,
            "system_id": 30_000_144,
            "state": "offline"
        }])
    );
}

}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod esi_corp_market_industry_ext {
    use super::*;

// End-to-end integration tests for ESI corporation role-gated routes, market history/prices/structures,
// and industry facilities/jobs (`esi:*` open-row closure).


use crate::esi_market_industry_pi::{issue_token_with_scopes, make_item, make_structure, rpc_call};
use eve_db::corp_roles::{
    ROLE_ACCOUNTANT, ROLE_DIRECTOR, ROLE_FACTORY_MANAGER, ROLE_STATION_MANAGER, ROLE_TRADER,
};
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::sample_routes::is_operation_implemented;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use glam::DVec3;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn call_esi(router: &EsiRouter, path: &str, token: Option<&str>) -> (u16, Value) {
    let mut req = EsiHttpRequest::new("GET", path);
    if let Some(tok) = token {
        req = req.with_bearer_token(tok);
    }
    let resp = router.handle_request(&req);
    let body: Value = serde_json::from_slice(&resp.body).unwrap_or(Value::Null);
    (resp.status_code, body)
}

#[tokio::test]
async fn test_esi_corp_role_gated_mining_customs_and_orders() {
    // parity: esi:GET /corporations/{corporation_id}/orders, esi:GET /corporations/{corporation_id}/orders/history
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/orders",
        "esi:GET /corporations/{corporation_id}/orders/history",
    ]);
    assert!(is_operation_implemented("GET", "/corporation/{corporation_id}/mining/extractions"));
    assert!(is_operation_implemented("GET", "/corporation/{corporation_id}/mining/observers"));
    assert!(is_operation_implemented(
        "GET",
        "/corporation/{corporation_id}/mining/observers/{observer_id}"
    ));
    assert!(is_operation_implemented("GET", "/corporations/{corporation_id}/customs_offices"));

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));

    let corp_id = 98_500_001_i64;
    let ceo_id = 90_000_001_i64;
    let station_mgr_id = 90_000_002_i64;
    let accountant_id = 90_000_003_i64;
    let trader_id = 90_000_004_i64;
    let plain_member_id = 90_000_005_i64;
    let observer_id = 1_030_000_000_001_i64;
    let ship_id = 100_000_001_i64;
    let jita_station = 60_003_760_i64;
    let lowsec_sys = 30_000_192_u32;
    let lowsec_planet_id = ctx.sde.solar_systems[&lowsec_sys].planet_ids[0];
    let perim_moon = ctx
        .sde
        .celestials_in_system(30_000_144)
        .into_iter()
        .find(|c| c.group_id == 8)
        .expect("SDE moon in Perimeter");
    let perim_pos = [
        perim_moon.position.x + 2_000.0,
        perim_moon.position.y,
        perim_moon.position.z,
    ];

    ctx.db
        .try_with_mut_snapshot(|snap| {
            let member_roles = HashMap::from([
                (
                    ceo_id,
                    ROLE_DIRECTOR | ROLE_STATION_MANAGER | ROLE_ACCOUNTANT | ROLE_TRADER,
                ),
                (station_mgr_id, ROLE_STATION_MANAGER),
                (accountant_id, ROLE_ACCOUNTANT),
                (trader_id, ROLE_TRADER),
                (plain_member_id, 0),
            ]);

            let template_char = snap.characters[&ceo_id].clone();
            for cid in [ceo_id, station_mgr_id, accountant_id, trader_id, plain_member_id] {
                let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| {
                    let mut c = template_char.clone();
                    c.character_id = cid;
                    c.name = format!("Pilot {cid}");
                    c
                });
                ch.corporation_id = corp_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = 30_000_144;
                ch.active_ship_id = ship_id;
                ch.balance = IskCents::from_isk(250_000_000);
                snap.characters.insert(cid, ch);
            }

            let mut corp = { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Role Gate Test Corp".into();
                __corp.ticker = "RGTC".into();
                __corp.ceo_id = ceo_id;
                __corp.tax_rate = 0.05;
                __corp.members = HashSet::from([
                    ceo_id,
                    station_mgr_id,
                    accountant_id,
                    trader_id,
                    plain_member_id,
                ]);
                __corp.member_roles = member_roles;
                __corp.hq_station_id = Some(jita_station);
                __corp.set_division_balance(1, IskCents::from_isk(250_000_000));
                __corp };
            corp.ensure_wallet_divisions();
            for d in &mut corp.wallet_divisions {
                d.balance = IskCents::from_isk(250_000_000);
            }
            snap.corporations.insert(corp_id, corp);

            let mut ref_st = make_structure(observer_id, 35835, corp_id, 30_000_144, "Corp Moon Refinery");
            ref_st.position = perim_pos;
            ref_st.fuel_blocks = 1_000;
            snap.structures.insert(observer_id, ref_st);
            snap.items.insert(
                960_001,
                make_item(960_001, 45_009, corp_id, observer_id, InventoryFlag::ServiceSlot0, 1, true),
            );
            snap.module_online_states.insert(960_001, true);
            Ok::<(), ()>(())
        })
        .unwrap();

    // Populate moon extraction & observer mining ledger via live RPCs
    let mut ceo_sess = SessionState::new(9861);
    ceo_sess.user_id = Some(1);
    ceo_sess.character_id = Some(ceo_id);
    ceo_sess.corporation_id = Some(corp_id);
    ceo_sess.corp_role = ROLE_DIRECTOR | ROLE_STATION_MANAGER | ROLE_ACCOUNTANT | ROLE_TRADER;
    ceo_sess.solar_system_id = Some(30_000_144);
    ceo_sess.ship_id = Some(ship_id);

    let ext_args = [PyRep::Integer(observer_id), PyRep::Integer(518_400)];
    rpc_call(&ctx, &mut ceo_sess, "moonExtractions", "StartNewExtraction", &ext_args).await;
    ctx.db.clock().advance_secs(520_000);
    let frac = rpc_call(
        &ctx,
        &mut ceo_sess,
        "moonExtractions",
        "FractureChunkWithStructure",
        &[PyRep::Integer(observer_id)],
    )
    .await;
    let ast_id = match frac.dict_get("asteroidIDs") {
        Some(PyRep::List(v)) => v[0].as_i64().expect("fractured asteroid ID"),
        other => panic!("unexpected asteroidIDs: {other:?}"),
    };
    let _ = ctx
        .systems
        .get_or_spawn(30_000_144)
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 606,
            position: DVec3::from_array(perim_pos),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&ceo_id) {
            let mut ch = snap.characters.values().next().unwrap().clone();
            ch.character_id = ceo_id;
            ch.corporation_id = corp_id;
            ch.solar_system_id = 30_000_144;
            ch.active_ship_id = ship_id;
            snap.characters.insert(ceo_id, ch);
        }
        snap.skills.entry(ceo_id).or_default().insert(3386, eve_db::TrainedSkillRecord { type_id: 3386, level: 5, skill_points: 256_000.0 });
        snap.items.insert(ship_id, make_item(ship_id, 606, ceo_id, 30_000_144, InventoryFlag::None, 1, true));
        snap.items.insert(92_000_483, make_item(92_000_483, 483, ceo_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(92_000_483, true);
    });
    ctx.invalidate_ship_fitting(ship_id);
    rpc_call(&ctx, &mut ceo_sess, "dogmaIM", "Mine", &[PyRep::Integer(ast_id)]).await;
    rpc_call(&ctx, &mut ceo_sess, "moonExtractions", "StartNewExtraction", &ext_args).await;

    // Populate active & historical corporate orders via marketProxy.PlaceBuyOrder + CancelCharOrder
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.station_id = Some(jita_station);
            ch.solar_system_id = 30_000_142;
        }
    });
    ceo_sess.station_id = Some(jita_station);
    ceo_sess.solar_system_id = Some(30_000_142);
    ceo_sess.region_id = Some(10_000_002);
    let corp_buy_args = [
        PyRep::Integer(jita_station),
        PyRep::Integer(36),
        PyRep::Real(25.0),
        PyRep::Integer(1_000),
        PyRep::Integer(-1),
        PyRep::Integer(1),
        PyRep::Integer(90),
        PyRep::Bool(true),
        PyRep::Integer(1000),
    ];
    let active_rep = rpc_call(&ctx, &mut ceo_sess, "marketProxy", "PlaceBuyOrder", &corp_buy_args).await;
    let active_oid = active_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    let hist_rep = rpc_call(&ctx, &mut ceo_sess, "marketProxy", "PlaceBuyOrder", &corp_buy_args).await;
    let hist_oid = hist_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    rpc_call(&ctx, &mut ceo_sess, "marketProxy", "CancelCharOrder", &[PyRep::Integer(hist_oid)])
        .await;

    // Populate customs office via posMgr.AnchorOrbital + OnlineOrbital + planetOrbitalRegistryBroker.UpdateSettings
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.station_id = None;
            ch.solar_system_id = lowsec_sys;
            ch.standings.insert(500_026, 2.0);
        }
        snap.items
            .insert(960_002, make_item(960_002, 3962, corp_id, ship_id, InventoryFlag::Cargo, 1, false));
        snap.items
            .insert(960_003, make_item(960_003, 2233, corp_id, ship_id, InventoryFlag::Cargo, 1, false));
    });
    ceo_sess.station_id = None;
    ceo_sess.solar_system_id = Some(lowsec_sys);
    let lowsec_planet = ctx.sde.celestials_in_system(lowsec_sys).into_iter().find(|c| c.celestial_id == lowsec_planet_id).unwrap();
    let near_planet = lowsec_planet.position + glam::DVec3::new(lowsec_planet.radius + 5_000.0, 0.0, 0.0);
    ctx.systems.get_or_spawn(lowsec_sys).spawn_ball(eve_destiny::Ball {
        id: ship_id,
        type_id: 587,
        position: near_planet,
        velocity: glam::DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    }).await.unwrap();
    rpc_call(
        &ctx,
        &mut ceo_sess,
        "posMgr",
        "AnchorOrbital",
        &[PyRep::Integer(960_002), PyRep::Integer(lowsec_planet_id)],
    )
    .await;
    rpc_call(
        &ctx,
        &mut ceo_sess,
        "posMgr",
        "OnlineOrbital",
        &[PyRep::Integer(960_002), PyRep::Integer(960_003)],
    )
    .await;
    let poco_args = [
        PyRep::Integer(lowsec_planet_id),
        PyRep::Integer(18),
        PyRep::keyval([
            ("corporation", PyRep::Real(0.01)),
            ("alliance", PyRep::Real(0.02)),
            ("standingHigh", PyRep::Real(0.05)),
            ("standingGood", PyRep::Real(0.08)),
            ("standingNeutral", PyRep::Real(0.10)),
            ("standingBad", PyRep::Real(0.15)),
            ("standingHorrible", PyRep::Real(0.20)),
        ]),
        PyRep::Real(0.0),
        PyRep::Bool(true),
        PyRep::Bool(false),
    ];
    rpc_call(&ctx, &mut ceo_sess, "planetOrbitalRegistryBroker", "UpdateSettings", &poco_args)
        .await;

    let all_scopes = vec![
        "esi-industry.read_corporation_mining.v1",
        "esi-planets.read_customs_offices.v1",
        "esi-markets.read_corporation_orders.v1",
    ];
    let sm_tok = issue_token_with_scopes(&ctx, station_mgr_id, all_scopes.clone());
    let acc_tok = issue_token_with_scopes(&ctx, accountant_id, all_scopes.clone());
    let trd_tok = issue_token_with_scopes(&ctx, trader_id, all_scopes.clone());
    let dir_tok = issue_token_with_scopes(&ctx, ceo_id, all_scopes.clone());
    let mem_tok = issue_token_with_scopes(&ctx, plain_member_id, all_scopes);

    // 1. Station_Manager allowed on mining/extractions; Accountant & plain member rejected with 403
    let (st, body) = call_esi(
        &router,
        &format!("/v1/corporation/{corp_id}/mining/extractions/"),
        Some(&sm_tok),
    );
    assert_eq!(st, 200);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(
        call_esi(
            &router,
            &format!("/v1/corporation/{corp_id}/mining/extractions/"),
            Some(&acc_tok),
        )
        .0,
        403
    );

    // 2. Accountant allowed on mining/observers & mining/observers/{id}; Station_Manager rejected with 403
    let (st, obs) = call_esi(
        &router,
        &format!("/v1/corporation/{corp_id}/mining/observers/"),
        Some(&acc_tok),
    );
    assert_eq!(st, 200);
    assert_eq!(obs[0]["observer_id"], json!(observer_id));
    let (st, obs_detail) = call_esi(
        &router,
        &format!("/v1/corporation/{corp_id}/mining/observers/{observer_id}/"),
        Some(&acc_tok),
    );
    assert_eq!(st, 200);
    assert!(obs_detail[0]["quantity"].as_i64().unwrap() > 0);
    assert_eq!(
        call_esi(
            &router,
            &format!("/v1/corporation/{corp_id}/mining/observers/"),
            Some(&sm_tok),
        )
        .0,
        403
    );

    // 3. Accountant and Trader allowed on corporations/{id}/orders & orders/history; Station_Manager rejected
    for tok in [&acc_tok, &trd_tok] {
        let (st, orders) = call_esi(
            &router,
            &format!("/v1/corporations/{corp_id}/orders/"),
            Some(tok),
        );
        assert_eq!(st, 200);
        assert_eq!(orders[0]["order_id"], json!(active_oid));
        let (st_h, hist) = call_esi(
            &router,
            &format!("/v1/corporations/{corp_id}/orders/history/"),
            Some(tok),
        );
        assert_eq!(st_h, 200);
        assert_eq!(hist[0]["order_id"], json!(hist_oid));
    }
    assert_eq!(
        call_esi(&router, &format!("/v1/corporations/{corp_id}/orders/"), Some(&sm_tok)).0,
        403
    );

    // 4. Director allowed on customs_offices; plain member rejected with 403
    let (st, pocos) = call_esi(
        &router,
        &format!("/v1/corporations/{corp_id}/customs_offices/"),
        Some(&dir_tok),
    );
    assert_eq!(st, 200);
    assert_eq!(pocos[0]["system_id"], json!(lowsec_sys));
    assert_eq!(
        call_esi(
            &router,
            &format!("/v1/corporations/{corp_id}/customs_offices/"),
            Some(&mem_tok),
        )
        .0,
        403
    );
}

#[tokio::test]
async fn test_esi_market_history_prices_structures_and_industry_jobs() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));

    let seller_id = 90_000_001_i64;
    let buyer_id = 90_000_002_i64;
    let corp_id = 98_500_002_i64;
    let jita_station = 60_003_760_i64;
    let jita_system = 30_000_142_u32;
    let forge_region = 10_000_002_u32;
    let structure_id = 1_050_000_000_001_i64;
    let trit_item_id = 99_910_001_i64;
    let bpc_item_id = 99_910_002_i64;

    // 1. GET /industry/facilities includes SDE NPC industry stations with 0.0025 tax even with 0 active jobs
    let (st, facs) = call_esi(&router, "/v1/industry/facilities/", None);
    assert_eq!(st, 200);
    let jita_fac = facs
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["facility_id"] == json!(jita_station))
        .expect("Jita 4-4 NPC factory facility");
    assert_eq!(jita_fac["tax"], json!(0.0025));

    // 2. Setup corp, structure without Market Hub, items, and wallets
    ctx.db
        .try_with_mut_snapshot(|snap| {
            let member_roles = HashMap::from([(seller_id, ROLE_DIRECTOR | ROLE_FACTORY_MANAGER)]);
            if let Some(ch) = snap.characters.get_mut(&seller_id) {
                ch.corporation_id = corp_id;
                ch.station_id = Some(jita_station);
                ch.solar_system_id = jita_system;
                ch.balance = IskCents::from_isk(100_000_000);
            }
            if let Some(ch) = snap.characters.get_mut(&buyer_id) {
                ch.station_id = Some(jita_station);
                ch.solar_system_id = jita_system;
                ch.balance = IskCents::from_isk(100_000_000);
            }
            snap.corporations.insert(
                corp_id,
                { let mut __corp = CorporationRecord::default();
                    __corp.corporation_id = corp_id;
                    __corp.name = "Industry Market Corp".into();
                    __corp.ticker = "IMC".into();
                    __corp.ceo_id = seller_id;
                    __corp.tax_rate = 0.05;
                    __corp.members = HashSet::from([seller_id]);
                    __corp.member_roles = member_roles;
                    __corp.hq_station_id = Some(jita_station);
                    __corp.set_division_balance(1, IskCents::from_isk(100_000_000));
                    __corp },
            );
            snap.structures.insert(
                structure_id,
                make_structure(structure_id, 35833, corp_id, jita_system, "Jita Fortizar"),
            );
            snap.items.insert(
                trit_item_id,
                make_item(trit_item_id, 39, seller_id, jita_station, InventoryFlag::Hangar, 50_000, false),
            );
            snap.items.insert(
                bpc_item_id,
                InventoryItemRecord {
                    item_id: bpc_item_id,
                    type_id: 691, // Rifter Blueprint
                    owner_id: seller_id,
                    location_id: jita_station,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    is_blueprint_copy: true,
                    blueprint_runs_remaining: Some(5),
                    ..Default::default()
                },
            );
            for (idx, (tid, qty)) in [
                (34_u32, 30_000_i32),
                (35, 10_000),
                (36, 3_000),
                (37, 1_000),
                (38, 5_000),
                (39, 5_000),
                (40, 5_000),
                (57_478, 200),
                (57_479, 10),
                (57_486, 100),
            ]
            .into_iter()
            .enumerate()
            {
                let iid = 99_910_010 + idx as i64;
                snap.items.insert(
                    iid,
                    make_item(iid, tid, seller_id, jita_station, InventoryFlag::Hangar, qty, false),
                );
            }
            Ok::<(), ()>(())
        })
        .unwrap();

    // 3. Bare structure without Standup Market Hub I must return 403 on GET /markets/structures/{id}
    let mkt_tok = issue_token_with_scopes(&ctx, seller_id, vec!["esi-markets.structure_markets.v1"]);
    assert_eq!(
        call_esi(
            &router,
            &format!("/v1/markets/structures/{structure_id}/"),
            Some(&mkt_tok),
        )
        .0,
        403
    );
    // Fit and online Standup Market Hub I (type_id 35892) -> 200 OK
    ctx.db
        .try_with_mut_snapshot(|snap| {
            snap.items.insert(
                99_910_099,
                make_item(99_910_099, 35892, corp_id, structure_id, InventoryFlag::ServiceSlot0, 1, true),
            );
            Ok::<(), ()>(())
        })
        .unwrap();
    assert_eq!(
        call_esi(
            &router,
            &format!("/v1/markets/structures/{structure_id}/"),
            Some(&mkt_tok),
        )
        .0,
        200
    );

    // 4. Execute real market orders via marketProxy and verify GET /markets/{region_id}/history & /markets/prices
    let mut seller_sess = SessionState::default();
    seller_sess.user_id = Some(1);
    seller_sess.role = eve_db::accounts::service_roles::ROLE_PLAYER_DEFAULT;
    seller_sess.character_id = Some(seller_id);
    seller_sess.corporation_id = Some(corp_id);
    seller_sess.station_id = Some(jita_station);
    seller_sess.solar_system_id = Some(jita_system);

    let mut buyer_sess = SessionState::default();
    buyer_sess.user_id = Some(1);
    buyer_sess.role = eve_db::accounts::service_roles::ROLE_PLAYER_DEFAULT;
    buyer_sess.character_id = Some(buyer_id);
    buyer_sess.station_id = Some(jita_station);
    buyer_sess.solar_system_id = Some(jita_system);

    let sell_order_arg = PyRep::List(vec![PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(trit_item_id)),
        (PyRep::String("typeID".into()), PyRep::Integer(39)),
        (PyRep::String("price".into()), PyRep::Real(7.5)),
        (PyRep::String("quantity".into()), PyRep::Integer(10_000)),
        (PyRep::String("stationID".into()), PyRep::Integer(jita_station)),
        (PyRep::String("duration".into()), PyRep::Integer(90)),
    ])]);
    rpc_call(
        &ctx,
        &mut seller_sess,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[sell_order_arg, PyRep::Bool(false)],
    )
    .await;
    let buy_args = [
        PyRep::Integer(jita_station),
        PyRep::Integer(39),
        PyRep::Real(7.5),
        PyRep::Integer(4_000),
        PyRep::Integer(-1),
        PyRep::Integer(1),
        PyRep::Integer(90),
        PyRep::Bool(false),
    ];
    rpc_call(&ctx, &mut buyer_sess, "marketProxy", "PlaceBuyOrder", &buy_args).await;

    let (st, hist) = call_esi(
        &router,
        &format!("/v1/markets/{forge_region}/history/?type_id=39"),
        None,
    );
    assert_eq!(st, 200);
    let hist_arr = hist.as_array().unwrap();
    assert!(!hist_arr.is_empty());
    assert_eq!(hist_arr[0]["volume"], json!(4_500));
    let hist_avg = hist_arr[0]["average"].as_f64().unwrap();
    assert!(hist_avg > 7.5);

    let (st, prices) = call_esi(&router, "/v1/markets/prices/", None);
    assert_eq!(st, 200);
    let trit_p = prices
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["type_id"] == json!(39))
        .expect("Zydrine in /markets/prices");
    assert_eq!(trit_p["average_price"], json!(hist_avg));
    assert_eq!(trit_p["adjusted_price"], json!(hist_avg));

    // 5. Install and deliver personal & corporation industry jobs via industryManager and verify isolation
    let personal_dict = PyRep::Dict(vec![
        (PyRep::String("blueprintID".into()), PyRep::Integer(bpc_item_id)),
        (PyRep::String("activityID".into()), PyRep::Integer(1)),
        (PyRep::String("runs".into()), PyRep::Integer(1)),
        (PyRep::String("facilityID".into()), PyRep::Integer(jita_station)),
        (PyRep::String("useCorp".into()), PyRep::Bool(false)),
    ]);
    let personal_rep =
        rpc_call(&ctx, &mut seller_sess, "industryManager", "InstallJob", &[personal_dict]).await;
    let job_id = personal_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("jobID");
    ctx.db.clock().advance_secs(10_000);
    rpc_call(&ctx, &mut seller_sess, "industryManager", "CompleteJob", &[PyRep::Integer(job_id)]).await;

    let corp_dict = PyRep::Dict(vec![
        (PyRep::String("blueprintID".into()), PyRep::Integer(bpc_item_id)),
        (PyRep::String("activityID".into()), PyRep::Integer(1)),
        (PyRep::String("runs".into()), PyRep::Integer(1)),
        (PyRep::String("facilityID".into()), PyRep::Integer(jita_station)),
        (PyRep::String("useCorp".into()), PyRep::Bool(true)),
    ]);
    let corp_rep =
        rpc_call(&ctx, &mut seller_sess, "industryManager", "InstallJob", &[corp_dict]).await;
    let corp_job_id = corp_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("corp jobID");
    ctx.db.clock().advance_secs(10_000);
    rpc_call(&ctx, &mut seller_sess, "industryManager", "CompleteJob", &[PyRep::Integer(corp_job_id)]).await;

    let ind_tok = issue_token_with_scopes(
        &ctx,
        seller_id,
        vec![
            "esi-industry.read_character_jobs.v1",
            "esi-industry.read_corporation_jobs.v1",
        ],
    );
    let (st, char_jobs) = call_esi(
        &router,
        &format!("/v1/characters/{seller_id}/industry/jobs/"),
        Some(&ind_tok),
    );
    assert_eq!(st, 200);
    let char_arr = char_jobs.as_array().unwrap();
    let cj = char_arr
        .iter()
        .find(|j| j["job_id"] == json!(job_id))
        .expect("delivered personal job in character industry/jobs");
    assert!(!char_arr.iter().any(|j| j["job_id"] == json!(corp_job_id)));
    assert_eq!(cj["status"], json!("delivered"));
    assert_eq!(cj["blueprint_location_id"], json!(jita_station));
    assert_eq!(cj["output_location_id"], json!(jita_station));
    assert!(cj["cost"].as_f64().unwrap_or(0.0) > 0.0);
    assert!(cj["completed_date"].as_str().is_some());
    assert_eq!(cj["completed_character_id"], json!(seller_id));

    let (st, corp_jobs) = call_esi(
        &router,
        &format!("/v1/corporations/{corp_id}/industry/jobs/"),
        Some(&ind_tok),
    );
    assert_eq!(st, 200);
    let corp_arr = corp_jobs.as_array().unwrap();
    let crj = corp_arr
        .iter()
        .find(|j| j["job_id"] == json!(corp_job_id))
        .expect("delivered corp job in corp industry/jobs");
    assert!(!corp_arr.iter().any(|j| j["job_id"] == json!(job_id)));
    assert_eq!(crj["location_id"], json!(jita_station));
    assert_eq!(crj["completed_character_id"], json!(seller_id));
}

}
