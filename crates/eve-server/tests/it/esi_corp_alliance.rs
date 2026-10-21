//! `ESI-5` integration tests for `Alliance` and `Corporation` OpenAPI endpoints and
//! companion `feat:esi.alliance.*` / `feat:esi.corporation.*` parity rows backed by
//! `WorldRepository` (`WorldStateSnapshot`) and `SdeRegistry`.

#![forbid(unsafe_code)]

use eve_db::{
    AllianceRecord, CharacterLoginStats, CorpWalletDivisionRecord, CorporationRecord,
    InventoryFlag, InventoryItemRecord, IskCents, StandingTransactionRecord, TrainedSkillRecord,
    UpwellStructureRecord,
};
use eve_server::esi::sample_routes::format_iso8601_utc;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token_with_scopes(ctx: &ServerContext, character_id: i64, scopes: Vec<String>) -> String {
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

fn check_cached_get(
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

    // 6. GET /corporations/{corporation_id}/icons (`esi:GET /corporations/{corporation_id}/icons`, `feat:esi.corporation.icons`)
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
    // parity: esi:GET /corporations/{corporation_id}/members, esi:GET /corporations/{corporation_id}/members/limit
    // parity: esi:GET /corporations/{corporation_id}/membertracking, esi:GET /corporations/{corporation_id}/roles
    // parity: feat:esi.corporation.membertracking
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/members",
        "esi:GET /corporations/{corporation_id}/members/limit",
        "esi:GET /corporations/{corporation_id}/membertracking",
        "esi:GET /corporations/{corporation_id}/roles",
        "feat:esi.corporation.membertracking",
    ]);

    let ctx = Arc::new(ServerContext::new());
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

        // Train Corporation Management V (type 3363, +20 members/lvl = 100) on CEO
        snap.skills.entry(ceo_id).or_default().insert(
            3363,
            TrainedSkillRecord {
                type_id: 3363,
                level: 5,
                skill_points: 256_000.0,
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

    // 1. Members & Member Limit (`GET /corporations/{id}/members`, `/limit`)
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

    let limit = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/members/limit/"))
            .with_bearer_token(&token),
        200,
        false,
    );
    let sde_bonus_per_lvl = ctx
        .sde
        .types
        .get(&3363)
        .and_then(|t| t.default_attributes.get(&191))
        .copied()
        .expect("SDE Corporation Management (3363) attribute 191")
        .round() as i64;
    assert_eq!(limit, json!(sde_bonus_per_lvl * 5));

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

    // 3. Roles (`GET /corporations/{id}/roles`)
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
    assert_eq!(r_arr[1]["character_id"], json!(wingman_id));
    assert_eq!(r_arr[1]["roles"], json!(["Accountant", "Factory_Manager"]));
}

#[tokio::test]
async fn test_esi5_corporation_divisions_blueprints_and_standings() {
    // parity: esi:GET /corporations/{corporation_id}/blueprints, esi:GET /corporations/{corporation_id}/divisions
    // parity: esi:GET /corporations/{corporation_id}/standings, feat:esi.corporation.blueprints
    // parity: feat:esi.corporation.divisions, feat:esi.corporation.standings
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/blueprints",
        "esi:GET /corporations/{corporation_id}/divisions",
        "esi:GET /corporations/{corporation_id}/standings",
        "feat:esi.corporation.blueprints",
        "feat:esi.corporation.divisions",
        "feat:esi.corporation.standings",
    ]);

    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let corp_id = 98_500_020_i64;
    let bpo_id = 885_101_i64;
    let bpc_id = 885_102_i64;
    let (&sde_agent_id, _) = ctx.sde.agents.iter().next().expect("SDE agent");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.user_id = 1;
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Kaalakiota Industrial Foundry".to_string(),
                ticker: "KAAL".to_string(),
                ceo_id,
                tax_rate: 0.04,
                wallet_divisions: vec![
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
                ],
                members: HashSet::from([ceo_id]),
                hq_station_id: Some(60_003_760),
                ..Default::default()
            },
        );

        // Researched BPO (ME 8, TE 16) in Hangar (CorpSAG1) and BPC (5 runs remaining) in Deliveries (CorpDeliveries)
        let mut bpo_attrs = HashMap::new();
        bpo_attrs.insert(1112, 8.0);
        bpo_attrs.insert(1113, 16.0);
        snap.items.insert(
            bpo_id,
            InventoryItemRecord {
                item_id: bpo_id,
                type_id: 688, // Rifter Blueprint
                owner_id: corp_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                dynamic_attributes: bpo_attrs,
                ..Default::default()
            },
        );
        snap.items.insert(
            bpc_id,
            InventoryItemRecord {
                item_id: bpc_id,
                type_id: 688,
                owner_id: corp_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Deliveries,
                quantity: 1,
                singleton: true,
                is_blueprint_copy: true,
                blueprint_runs_remaining: Some(5),
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
    assert_eq!(bp_arr.len(), 2);
    assert_eq!(bp_arr[0]["item_id"], json!(bpo_id));
    assert_eq!(bp_arr[0]["type_id"], json!(688));
    assert_eq!(bp_arr[0]["location_flag"], json!("CorpSAG1"));
    assert_eq!(bp_arr[0]["quantity"], json!(-1));
    assert_eq!(bp_arr[0]["runs"], json!(-1));
    assert_eq!(bp_arr[0]["material_efficiency"], json!(8));
    assert_eq!(bp_arr[0]["time_efficiency"], json!(16));
    assert_eq!(bp_arr[1]["item_id"], json!(bpc_id));
    assert_eq!(bp_arr[1]["location_flag"], json!("CorpDeliveries"));
    assert_eq!(bp_arr[1]["quantity"], json!(-2));
    assert_eq!(bp_arr[1]["runs"], json!(5));
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

    // 2. Divisions (`GET /corporations/{id}/divisions`, `feat:esi.corporation.divisions`)
    let divs = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/divisions/"))
            .with_bearer_token(&token),
        200,
        false,
    );
    assert_eq!(
        divs["wallet"],
        json!([
            { "division": 1, "name": "Capital Reserve" },
            { "division": 2, "name": "R&D Operations" }
        ])
    );
    assert_eq!(
        divs["hangar"],
        json!([
            { "division": 1, "name": "Capital Reserve" },
            { "division": 2, "name": "R&D Operations" }
        ])
    );

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
    // parity: esi:GET /corporations/{corporation_id}/structures, feat:esi.corporation.facilities
    // parity: feat:esi.corporation.starbases, feat:esi.corporation.structures
    // parity: feat:esi.corporations.structures-services-timers
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/facilities",
        "esi:GET /corporations/{corporation_id}/starbases",
        "esi:GET /corporations/{corporation_id}/structures",
        "feat:esi.corporation.facilities",
        "feat:esi.corporation.starbases",
        "feat:esi.corporation.structures",
        "feat:esi.corporations.structures-services-timers",
    ]);

    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let corp_id = 98_500_030_i64;
    let struct_id = 1_035_000_030_001_i64;
    let svc_online_id = 885_201_i64;
    let svc_offline_id = 885_202_i64;
    let starbase_id = 885_203_i64;
    let timer_start_unix = 1_710_000_000_u64;
    let timer_end_unix = 1_710_010_800_u64;
    let next_apply_unix = 1_710_090_000_u64; // (1_710_090_000 / 3600) % 24 == 17

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.user_id = 1;
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Upwell Deep Space Engineering".to_string(),
                ticker: "UDSE".to_string(),
                ceo_id,
                tax_rate: 0.05,
                members: HashSet::from([ceo_id]),
                hq_station_id: Some(60_003_760),
                ..Default::default()
            },
        );

        // Owned Upwell structure in ArmorReinforced state with vulnerability window & 2 fitted service modules
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_825, // Raitaru
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
            },
        );
        snap.structure_vulnerability_windows
            .insert(struct_id, (16 * 3600, next_apply_unix));

        // ServiceSlot0: Standup Cloning Center I (35894, online); ServiceSlot1: Standup Market Hub I (35892, offline)
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
        snap.module_online_states.insert(svc_offline_id, false);
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

    // 1. Facilities (`GET /corporations/{id}/facilities`, `feat:esi.corporation.facilities`)
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

    // 2. Structures (`GET /corporations/{id}/structures`, `feat:esi.corporation.structures`, `feat:esi.corporations.structures-services-timers`)
    let structures = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v4/corporations/{corp_id}/structures/"))
            .with_bearer_token(&token),
        200,
        true,
    );
    let st_arr = structures.as_array().expect("structures array");
    assert_eq!(st_arr.len(), 1);
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
    assert_eq!(
        st["fuel_expires"],
        json!(format_iso8601_utc(timer_start_unix + 300 * 3600))
    );

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

    // 3. Starbases (`GET /corporations/{id}/starbases`, `feat:esi.corporation.starbases`)
    // Empty in V24 when no POS Control Tower is anchored:
    let sb_empty = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/starbases/"))
            .with_bearer_token(&token),
        200,
        true,
    );
    assert_eq!(sb_empty, json!([]));

    // Anchor a category-23 POS Control Tower (12235 Amarr Control Tower) and verify live state
    assert_eq!(
        ctx.sde
            .types
            .get(&12_235)
            .expect("12235 in SDE")
            .category_id,
        23
    );
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
        snap.module_online_states.insert(starbase_id, false);
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
