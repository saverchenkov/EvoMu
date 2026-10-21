//! `ESI-5` integration tests for Corporation and Alliance governance, lifecycle metadata,
//! alliance history, container audit logs, hangar/wallet divisions, medals, member limits,
//! titles, 8-mask role assignments and role history, shareholders, starbase configuration
//! detail, Upwell structure timers/fuel expiry, and corporation projects (`plan:ESI-5`).

#![forbid(unsafe_code)]

use eve_db::{
    AllianceLifecycleMeta, AllianceRecord, CorpAllianceHistoryEntry, CorpContainerLogEntry,
    CorpIssuedMedalEntry, CorpMedalEntry, CorpMemberRoleAssignment, CorpProjectContributorEntry,
    CorpProjectRecord, CorpRoleHistoryEntry, CorpShareholderEntry, CorpTitleEntry,
    CorpWalletDivisionRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    StarbaseDetailSettings, StructureEsiTimerMeta, TrainedSkillRecord, UpwellStructureRecord,
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
            "evomu-esi5-corp-governance-test",
            "https://localhost/callback",
            scopes,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-esi5-corp-governance-test", PKCE_VERIFIER, now)
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
async fn test_esi5_alliance_info_history_divisions_and_shareholders() {
    // parity: esi:GET /alliances/{alliance_id}, feat:esi.alliance.info
    // parity: esi:GET /corporations/{corporation_id}/alliancehistory, feat:esi.corporation.alliance-history
    // parity: esi:GET /corporations/{corporation_id}/divisions, feat:esi.corporation.divisions
    // parity: esi:GET /corporations/{corporation_id}/shareholders, feat:esi.corporation.shareholders
    eve_testkit::attest(&[
        "esi:GET /alliances/{alliance_id}",
        "feat:esi.alliance.info",
        "esi:GET /corporations/{corporation_id}/alliancehistory",
        "feat:esi.corporation.alliance-history",
        "esi:GET /corporations/{corporation_id}/divisions",
        "feat:esi.corporation.divisions",
        "esi:GET /corporations/{corporation_id}/shareholders",
        "feat:esi.corporation.shareholders",
    ]);

    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let wingman_id = 90_000_002_i64;
    let corp_id = 98_600_001_i64;
    let second_corp_id = 98_600_002_i64;
    let alliance_id = 99_600_001_i64;
    let founded_unix = 1_700_000_000_u64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.user_id = 1;
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Ishukone Strategic Holdings".to_string(),
                ticker: "ISKH".to_string(),
                ceo_id,
                alliance_id: Some(alliance_id),
                tax_rate: 0.05,
                wallet_divisions: vec![
                    CorpWalletDivisionRecord {
                        division_id: 1,
                        name: "Master Treasury".to_string(),
                        balance: IskCents::from_isk(250_000_000),
                    },
                    CorpWalletDivisionRecord {
                        division_id: 2,
                        name: "SRP Escrow".to_string(),
                        balance: IskCents::from_isk(75_000_000),
                    },
                ],
                members: HashSet::from([ceo_id, wingman_id]),
                hq_station_id: Some(60_003_760),
                ..Default::default()
            },
        );
        snap.corp_hangar_divisions.insert(
            corp_id,
            HashMap::from([
                (1, "Ore & Minerals Bay".to_string()),
                (3, "Capital Components Vault".to_string()),
            ]),
        );
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "Caldari Industrial Pact".to_string(),
                short_name: "CIPC".to_string(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );
        snap.alliance_lifecycle_meta.insert(
            alliance_id,
            AllianceLifecycleMeta {
                creator_id: ceo_id,
                creator_corporation_id: corp_id,
                date_founded_unix: founded_unix,
                faction_id: Some(500_001),
            },
        );
        snap.corp_alliance_history.insert(
            corp_id,
            vec![
                CorpAllianceHistoryEntry {
                    record_id: 10,
                    alliance_id: None,
                    start_date_unix: 1_690_000_000,
                    is_deleted: None,
                },
                CorpAllianceHistoryEntry {
                    record_id: 25,
                    alliance_id: Some(alliance_id),
                    start_date_unix: founded_unix,
                    is_deleted: Some(false),
                },
            ],
        );
        snap.corp_shareholders.insert(
            corp_id,
            vec![
                CorpShareholderEntry {
                    shareholder_id: ceo_id,
                    shareholder_type: "character".to_string(),
                    share_count: 750,
                },
                CorpShareholderEntry {
                    shareholder_id: corp_id,
                    shareholder_type: "corporation".to_string(),
                    share_count: 250,
                },
            ],
        );
    });

    let token = issue_token_with_scopes(
        &ctx,
        ceo_id,
        vec![
            "esi-corporations.read_divisions.v1".to_string(),
            "esi-wallet.read_corporation_wallets.v1".to_string(),
        ],
    );

    // 1. GET /alliances/{alliance_id} (`esi:GET /alliances/{alliance_id}`, `feat:esi.alliance.info`)
    let al_info = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v3/alliances/{alliance_id}/")),
        200,
        false,
    );
    assert_eq!(al_info["name"], json!("Caldari Industrial Pact"));
    assert_eq!(al_info["ticker"], json!("CIPC"));
    assert_eq!(al_info["creator_id"], json!(ceo_id));
    assert_eq!(al_info["creator_corporation_id"], json!(corp_id));
    assert_eq!(al_info["executor_corporation_id"], json!(corp_id));
    assert_eq!(al_info["faction_id"], json!(500_001));
    assert_eq!(
        al_info["date_founded"],
        json!(format_iso8601_utc(founded_unix))
    );

    // Mutate executor_corp_id and verify live state reflection
    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances
            .get_mut(&alliance_id)
            .unwrap()
            .executor_corp_id = second_corp_id;
    });
    let al_info_updated = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v3/alliances/{alliance_id}/")),
        200,
        false,
    );
    assert_eq!(
        al_info_updated["executor_corporation_id"],
        json!(second_corp_id)
    );
    assert_eq!(
        check_cached_get(
            &router,
            &spec,
            &EsiHttpRequest::new("GET", "/v3/alliances/999999999/"),
            404,
            false,
        )["status"],
        json!(404)
    );

    // 2. GET /corporations/{corporation_id}/alliancehistory (`esi:GET /corporations/{corporation_id}/alliancehistory`, `feat:esi.corporation.alliance-history`)
    let history = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v2/corporations/{corp_id}/alliancehistory/")),
        200,
        false,
    );
    let hist_arr = history.as_array().expect("alliancehistory array");
    assert_eq!(hist_arr.len(), 2);
    assert_eq!(hist_arr[0]["record_id"], json!(25));
    assert_eq!(hist_arr[0]["alliance_id"], json!(alliance_id));
    assert_eq!(hist_arr[0]["is_deleted"], json!(false));
    assert_eq!(
        hist_arr[0]["start_date"],
        json!(format_iso8601_utc(founded_unix))
    );
    assert_eq!(hist_arr[1]["record_id"], json!(10));
    assert!(hist_arr[1].get("alliance_id").is_none());

    // 3. GET /corporations/{corporation_id}/divisions (`esi:GET /corporations/{corporation_id}/divisions`, `feat:esi.corporation.divisions`)
    let divs = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/divisions/"))
            .with_bearer_token(&token),
        200,
        false,
    );
    assert_eq!(
        divs["hangar"],
        json!([
            { "division": 1, "name": "Ore & Minerals Bay" },
            { "division": 3, "name": "Capital Components Vault" }
        ])
    );
    assert_eq!(
        divs["wallet"],
        json!([
            { "division": 1, "name": "Master Treasury" },
            { "division": 2, "name": "SRP Escrow" }
        ])
    );

    // 4. GET /corporations/{corporation_id}/shareholders (`esi:GET /corporations/{corporation_id}/shareholders`, `feat:esi.corporation.shareholders`)
    let holders = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/shareholders/"))
            .with_bearer_token(&token),
        200,
        true,
    );
    assert_eq!(
        holders,
        json!([
            {
                "shareholder_id": ceo_id,
                "shareholder_type": "character",
                "share_count": 750
            },
            {
                "shareholder_id": corp_id,
                "shareholder_type": "corporation",
                "share_count": 250
            }
        ])
    );
    assert_eq!(
        check_cached_get(
            &router,
            &spec,
            &EsiHttpRequest::new(
                "GET",
                format!("/v1/corporations/{corp_id}/shareholders/?page=2"),
            )
            .with_bearer_token(&token),
            404,
            false,
        )["status"],
        json!(404)
    );
}

#[tokio::test]
async fn test_esi5_corporation_medals_container_logs_and_projects() {
    // parity: esi:GET /corporations/{corporation_id}/containers/logs, feat:esi.corporation.container-logs
    // parity: esi:GET /corporations/{corporation_id}/medals, esi:GET /corporations/{corporation_id}/medals/issued
    // parity: feat:esi.corporation.medals, feat:esi.corporation.projects
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/containers/logs",
        "feat:esi.corporation.container-logs",
        "esi:GET /corporations/{corporation_id}/medals",
        "esi:GET /corporations/{corporation_id}/medals/issued",
        "feat:esi.corporation.medals",
        "feat:esi.corporation.projects",
    ]);

    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let wingman_id = 90_000_002_i64;
    let corp_id = 98_600_010_i64;
    let project_id = 700_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.user_id = 1;
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Hyasyoda Logistics Directorate".to_string(),
                ticker: "HYLD".to_string(),
                ceo_id,
                tax_rate: 0.05,
                members: HashSet::from([ceo_id, wingman_id]),
                hq_station_id: Some(60_003_760),
                ..Default::default()
            },
        );
        snap.corp_container_logs.insert(
            corp_id,
            vec![
                CorpContainerLogEntry {
                    logged_at_unix: 1_710_100_000,
                    container_id: 990_001,
                    container_type_id: 17_366, // Station Vault Container
                    character_id: ceo_id,
                    location_id: 60_003_760,
                    location_flag: "CorpSAG1".to_string(),
                    action: "lock".to_string(),
                    password_type: None,
                    type_id: Some(34),
                    quantity: Some(50_000),
                    old_config_bitmask: None,
                    new_config_bitmask: None,
                },
                CorpContainerLogEntry {
                    logged_at_unix: 1_710_200_000,
                    container_id: 990_001,
                    container_type_id: 17_366,
                    character_id: wingman_id,
                    location_id: 60_003_760,
                    location_flag: "CorpSAG1".to_string(),
                    action: "configure".to_string(),
                    password_type: Some("config".to_string()),
                    type_id: None,
                    quantity: None,
                    old_config_bitmask: Some(3),
                    new_config_bitmask: Some(7),
                },
            ],
        );
        snap.corp_medals.insert(
            corp_id,
            vec![CorpMedalEntry {
                medal_id: 501,
                title: "Star of Caldari Prime".to_string(),
                description: "Awarded for logistics excellence under blockade".to_string(),
                creator_id: ceo_id,
                created_at_unix: 1_705_000_000,
            }],
        );
        snap.corp_issued_medals.insert(
            corp_id,
            vec![CorpIssuedMedalEntry {
                medal_id: 501,
                character_id: wingman_id,
                reason: "Delivered 12 freighter convoys to Jita 4-4".to_string(),
                status: "public".to_string(),
                issuer_id: ceo_id,
                issued_at_unix: 1_706_000_000,
            }],
        );
        snap.corp_projects.insert(
            project_id,
            CorpProjectRecord {
                project_id,
                corporation_id: corp_id,
                name: "Deliver Tritanium for Citadel Hull".to_string(),
                target_quantity: 1_000_000,
                current_quantity: 450_000,
                isk_per_unit: IskCents::from_isk(5),
                escrow_remaining: IskCents::from_isk(2_750_000),
            },
        );
        snap.corp_project_contributors.insert(
            project_id,
            vec![
                CorpProjectContributorEntry {
                    character_id: ceo_id,
                    contributed_quantity: 200_000,
                },
                CorpProjectContributorEntry {
                    character_id: wingman_id,
                    contributed_quantity: 250_000,
                },
            ],
        );
    });

    let token = issue_token_with_scopes(
        &ctx,
        ceo_id,
        vec![
            "esi-corporations.read_container_logs.v1".to_string(),
            "esi-corporations.read_medals.v1".to_string(),
            "esi-corporations.read_projects.v1".to_string(),
        ],
    );

    // 1. Container Logs (`GET /corporations/{id}/containers/logs`, `feat:esi.corporation.container-logs`)
    let logs = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v2/corporations/{corp_id}/containers/logs/"),
        )
        .with_bearer_token(&token),
        200,
        true,
    );
    let log_arr = logs.as_array().expect("container logs array");
    assert_eq!(log_arr.len(), 2);
    assert_eq!(log_arr[0]["action"], json!("configure"));
    assert_eq!(log_arr[0]["character_id"], json!(wingman_id));
    assert_eq!(log_arr[0]["password_type"], json!("config"));
    assert_eq!(log_arr[0]["old_config_bitmask"], json!(3));
    assert_eq!(log_arr[0]["new_config_bitmask"], json!(7));
    assert_eq!(log_arr[1]["action"], json!("lock"));
    assert_eq!(log_arr[1]["type_id"], json!(34));
    assert_eq!(log_arr[1]["quantity"], json!(50_000));

    // 2. Medals & Issued Medals (`GET /corporations/{id}/medals`, `/medals/issued`, `feat:esi.corporation.medals`)
    let medals = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/medals/"))
            .with_bearer_token(&token),
        200,
        true,
    );
    assert_eq!(
        medals,
        json!([{
            "medal_id": 501,
            "title": "Star of Caldari Prime",
            "description": "Awarded for logistics excellence under blockade",
            "creator_id": ceo_id,
            "created_at": format_iso8601_utc(1_705_000_000)
        }])
    );

    let issued = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/medals/issued/"))
            .with_bearer_token(&token),
        200,
        true,
    );
    assert_eq!(
        issued,
        json!([{
            "medal_id": 501,
            "character_id": wingman_id,
            "reason": "Delivered 12 freighter convoys to Jita 4-4",
            "status": "public",
            "issuer_id": ceo_id,
            "issued_at": format_iso8601_utc(1_706_000_000)
        }])
    );

    // 3. Corporation Projects + Contributors (`GET /corporations/{id}/projects`, `feat:esi.corporation.projects`)
    let projects = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/projects/"))
            .with_bearer_token(&token),
        200,
        true,
    );
    assert_eq!(
        projects,
        json!([{
            "project_id": project_id,
            "corporation_id": corp_id,
            "name": "Deliver Tritanium for Citadel Hull",
            "target_quantity": 1_000_000,
            "current_quantity": 450_000,
            "isk_per_unit": 5.0,
            "escrow_remaining": 2_750_000.0,
            "contributors": [
                { "character_id": ceo_id, "contributed_quantity": 200_000 },
                { "character_id": wingman_id, "contributed_quantity": 250_000 }
            ]
        }])
    );
}

#[tokio::test]
async fn test_esi5_corporation_members_limit_titles_and_roles_history() {
    // parity: esi:GET /corporations/{corporation_id}/members/limit
    // parity: esi:GET /corporations/{corporation_id}/members/titles, esi:GET /corporations/{corporation_id}/titles
    // parity: feat:esi.corporation.members, feat:esi.corporation.titles
    // parity: esi:GET /corporations/{corporation_id}/roles, esi:GET /corporations/{corporation_id}/roles/history
    // parity: feat:esi.corporation.roles
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/members/limit",
        "esi:GET /corporations/{corporation_id}/members/titles",
        "esi:GET /corporations/{corporation_id}/titles",
        "feat:esi.corporation.members",
        "feat:esi.corporation.titles",
        "esi:GET /corporations/{corporation_id}/roles",
        "esi:GET /corporations/{corporation_id}/roles/history",
        "feat:esi.corporation.roles",
    ]);

    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let wingman_id = 90_000_002_i64;
    let corp_id = 98_600_020_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.user_id = 1;
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Lai Dai Directorate".to_string(),
                ticker: "LDD".to_string(),
                ceo_id,
                tax_rate: 0.05,
                members: HashSet::from([ceo_id, wingman_id]),
                hq_station_id: Some(60_003_760),
                ..Default::default()
            },
        );
        // Train Corporation Management IV (type 3363) on CEO
        snap.skills.entry(ceo_id).or_default().insert(
            3363,
            TrainedSkillRecord {
                type_id: 3363,
                level: 4,
                skill_points: 45_255.0,
            },
        );
        snap.corp_titles.insert(
            corp_id,
            vec![
                CorpTitleEntry {
                    title_id: 1,
                    name: "Chief Fleet Logistics".to_string(),
                    roles: vec!["Station_Manager".to_string()],
                    grantable_roles: vec!["Config_Equipment".to_string()],
                    roles_at_hq: vec!["Hangar_Take_1".to_string()],
                    grantable_roles_at_hq: vec!["Hangar_Query_1".to_string()],
                    roles_at_base: vec!["Container_Take_1".to_string()],
                    grantable_roles_at_base: vec![],
                    roles_at_other: vec!["Deliveries_Take".to_string()],
                    grantable_roles_at_other: vec![],
                },
                CorpTitleEntry {
                    title_id: 2,
                    name: "Industrial Foreman".to_string(),
                    roles: vec!["Factory_Manager".to_string()],
                    ..Default::default()
                },
            ],
        );
        snap.corp_member_titles
            .insert((corp_id, ceo_id), vec![1, 2]);
        snap.corp_member_titles.insert((corp_id, wingman_id), vec![2]);

        snap.corp_role_assignments.insert(
            (corp_id, ceo_id),
            CorpMemberRoleAssignment {
                character_id: ceo_id,
                roles: eve_db::corp_roles::ROLE_DIRECTOR,
                grantable_roles: eve_db::corp_roles::ROLE_PERSONNEL_MANAGER,
                roles_at_hq: eve_db::corp_roles::ROLE_STATION_MANAGER,
                grantable_roles_at_hq: eve_db::corp_roles::ROLE_CONFIG_EQUIPMENT,
                roles_at_base: eve_db::corp_roles::ROLE_FACTORY_MANAGER,
                grantable_roles_at_base: eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT,
                roles_at_other: eve_db::corp_roles::ROLE_SECURITY_OFFICER,
                grantable_roles_at_other: eve_db::corp_roles::ROLE_AUDITOR,
            },
        );
        snap.corp_role_history.insert(
            corp_id,
            vec![CorpRoleHistoryEntry {
                character_id: wingman_id,
                changed_at_unix: 1_710_500_000,
                issuer_id: ceo_id,
                role_type: "roles".to_string(),
                old_roles: vec!["Junior_Accountant".to_string()],
                new_roles: vec!["Accountant".to_string(), "Factory_Manager".to_string()],
            }],
        );
    });

    let token = issue_token_with_scopes(
        &ctx,
        ceo_id,
        vec![
            "esi-corporations.read_corporation_membership.v1".to_string(),
            "esi-corporations.track_members.v1".to_string(),
            "esi-corporations.read_titles.v1".to_string(),
        ],
    );

    // 1. Members & Member Limit (`GET /corporations/{id}/members`, `/members/limit`, `feat:esi.corporation.members`)
    let members = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v3/corporations/{corp_id}/members/"))
            .with_bearer_token(&token),
        200,
        false,
    );
    assert_eq!(members, json!([ceo_id, wingman_id]));

    let sde_bonus_per_lvl = ctx
        .sde
        .types
        .get(&3363)
        .and_then(|t| t.default_attributes.get(&191))
        .copied()
        .expect("SDE attribute 191 on skill 3363")
        .round() as i64;
    let derived_limit = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/members/limit/"))
            .with_bearer_token(&token),
        200,
        false,
    );
    assert_eq!(derived_limit, json!(sde_bonus_per_lvl * 4));

    // Override via explicit corp_member_limits and verify live update
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_member_limits.insert(corp_id, 650);
    });
    let explicit_limit = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/members/limit/"))
            .with_bearer_token(&token),
        200,
        false,
    );
    assert_eq!(explicit_limit, json!(650));

    // 2. Titles & Member Titles (`GET /corporations/{id}/titles`, `/members/titles`, `feat:esi.corporation.titles`)
    let titles = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/titles/"))
            .with_bearer_token(&token),
        200,
        false,
    );
    let titles_arr = titles.as_array().expect("titles array");
    assert_eq!(titles_arr.len(), 2);
    assert_eq!(titles_arr[0]["title_id"], json!(1));
    assert_eq!(titles_arr[0]["name"], json!("Chief Fleet Logistics"));
    assert_eq!(titles_arr[0]["roles"], json!(["Station_Manager"]));
    assert_eq!(titles_arr[0]["grantable_roles"], json!(["Config_Equipment"]));
    assert_eq!(titles_arr[0]["roles_at_hq"], json!(["Hangar_Take_1"]));
    assert_eq!(titles_arr[0]["roles_at_base"], json!(["Container_Take_1"]));
    assert_eq!(titles_arr[0]["roles_at_other"], json!(["Deliveries_Take"]));

    let member_titles = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/members/titles/"))
            .with_bearer_token(&token),
        200,
        false,
    );
    assert_eq!(
        member_titles,
        json!([
            { "character_id": ceo_id, "titles": [1, 2] },
            { "character_id": wingman_id, "titles": [2] }
        ])
    );

    // 3. Roles & Role History (`GET /corporations/{id}/roles`, `/roles/history`, `feat:esi.corporation.roles`)
    let roles = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/roles/"))
            .with_bearer_token(&token),
        200,
        false,
    );
    assert_eq!(
        roles,
        json!([{
            "character_id": ceo_id,
            "roles": ["Director"],
            "grantable_roles": ["Personnel_Manager"],
            "roles_at_hq": ["Station_Manager"],
            "grantable_roles_at_hq": ["Config_Equipment"],
            "roles_at_base": ["Factory_Manager"],
            "grantable_roles_at_base": ["Junior_Accountant"],
            "roles_at_other": ["Security_Officer"],
            "grantable_roles_at_other": ["Auditor"]
        }])
    );

    let role_history = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/roles/history/"))
            .with_bearer_token(&token),
        200,
        true,
    );
    assert_eq!(
        role_history,
        json!([{
            "character_id": wingman_id,
            "changed_at": format_iso8601_utc(1_710_500_000),
            "issuer_id": ceo_id,
            "role_type": "roles",
            "old_roles": ["Junior_Accountant"],
            "new_roles": ["Accountant", "Factory_Manager"]
        }])
    );
}

#[tokio::test]
async fn test_esi5_corporation_structures_timers_starbase_detail_and_plan_closure() {
    // parity: esi:GET /corporations/{corporation_id}/structures
    // parity: feat:esi.corporation.structures, feat:esi.corporations.structures-services-timers
    // parity: esi:GET /corporations/{corporation_id}/starbases/{starbase_id}
    // parity: plan:ESI-5
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/structures",
        "feat:esi.corporation.structures",
        "feat:esi.corporations.structures-services-timers",
        "esi:GET /corporations/{corporation_id}/starbases/{starbase_id}",
        "plan:ESI-5",
    ]);

    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let corp_id = 98_600_030_i64;
    let struct_id = 1_036_000_030_001_i64;
    let svc_online_id = 886_201_i64;
    let svc_offline_id = 886_202_i64;
    let starbase_id = 886_203_i64;
    let fuel_item_1 = 886_204_i64;
    let fuel_item_2 = 886_205_i64;
    let timer_start_unix = 1_710_000_000_u64;
    let timer_end_unix = 1_710_010_800_u64;
    let fuel_expires_unix = 1_711_800_000_u64;
    let unanchors_at_unix = 1_712_400_000_u64;
    let next_apply_unix = 1_710_090_000_u64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.user_id = 1;
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Upwell Consortium Operations".to_string(),
                ticker: "UPCO".to_string(),
                ceo_id,
                tax_rate: 0.05,
                members: HashSet::from([ceo_id]),
                hq_station_id: Some(60_003_760),
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_825, // Raitaru
                owner_corp_id: corp_id,
                solar_system_id: 30_000_144,
                name: "Perimeter Raitaru Foundry".to_string(),
                position: [10_000.0, 20_000.0, -30_000.0],
                state: "ArmorReinforced".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 0.0,
                armor_hp: 4_200.0,
                hull_hp: 5_000.0,
                reinforced_until_unix: timer_end_unix,
                last_damage_tick: 1,
                last_damage_unix: timer_start_unix,
            },
        );
        snap.structure_vulnerability_windows
            .insert(struct_id, (14 * 3600, next_apply_unix));
        snap.structure_esi_timer_meta.insert(
            struct_id,
            StructureEsiTimerMeta {
                profile_id: 42,
                fuel_expires_unix: Some(fuel_expires_unix),
                state_timer_start_unix: Some(timer_start_unix),
                state_timer_end_unix: Some(timer_end_unix),
                unanchors_at_unix: Some(unanchors_at_unix),
                next_reinforce_hour: Some(19),
                next_reinforce_apply_unix: Some(next_apply_unix),
            },
        );
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

        // Starbase Control Tower (12235 Amarr Control Tower, group 365) + fuel bay items + settings
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
            fuel_item_1,
            InventoryItemRecord {
                item_id: fuel_item_1,
                type_id: 4_051, // Nitrogen Fuel Block
                owner_id: corp_id,
                location_id: starbase_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 1_200,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            fuel_item_2,
            InventoryItemRecord {
                item_id: fuel_item_2,
                type_id: 16_275, // Strontium Clathrates
                owner_id: corp_id,
                location_id: starbase_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );
        snap.starbase_settings.insert(
            starbase_id,
            StarbaseDetailSettings {
                allow_corporation_members: true,
                allow_alliance_members: false,
                use_alliance_standings: true,
                attack_if_other_security_status_dropping: false,
                attack_if_at_war: true,
                fuel_bay_view: "corporation_member".to_string(),
                fuel_bay_take: "starbase_fuel_technician_role".to_string(),
                anchor: "config_starbase_equipment_role".to_string(),
                unanchor: "config_starbase_equipment_role".to_string(),
                online: "config_starbase_equipment_role".to_string(),
                offline: "config_starbase_equipment_role".to_string(),
                attack_standing_threshold: Some(-5.0),
                attack_security_status_threshold: Some(-2.5),
            },
        );
    });

    let token = issue_token_with_scopes(
        &ctx,
        ceo_id,
        vec![
            "esi-corporations.read_structures.v1".to_string(),
            "esi-corporations.read_starbases.v1".to_string(),
        ],
    );

    // 1. Structures (`GET /corporations/{id}/structures`, `feat:esi.corporation.structures`, `feat:esi.corporations.structures-services-timers`)
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
    assert_eq!(st["profile_id"], json!(42));
    assert_eq!(st["name"], json!("Perimeter Raitaru Foundry"));
    assert_eq!(st["state"], json!("armor_reinforce"));
    assert_eq!(st["reinforce_hour"], json!(14));
    assert_eq!(st["next_reinforce_hour"], json!(19));
    assert_eq!(
        st["fuel_expires"],
        json!(format_iso8601_utc(fuel_expires_unix))
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
        st["unanchors_at"],
        json!(format_iso8601_utc(unanchors_at_unix))
    );
    assert_eq!(
        st["next_reinforce_apply"],
        json!(format_iso8601_utc(next_apply_unix))
    );
    let sde_svc0_name = &ctx.sde.types.get(&35_894).expect("35894 in SDE").name;
    let sde_svc1_name = &ctx.sde.types.get(&35_892).expect("35892 in SDE").name;
    assert_eq!(
        st["services"],
        json!([
            { "name": sde_svc0_name, "state": "online" },
            { "name": sde_svc1_name, "state": "offline" }
        ])
    );

    // 2. Starbase Detail (`GET /corporations/{id}/starbases/{starbase_id}`)
    let sb_detail = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/corporations/{corp_id}/starbases/{starbase_id}/?system_id=30000144"),
        )
        .with_bearer_token(&token),
        200,
        false,
    );
    assert_eq!(sb_detail["allow_corporation_members"], json!(true));
    assert_eq!(sb_detail["allow_alliance_members"], json!(false));
    assert_eq!(sb_detail["use_alliance_standings"], json!(true));
    assert_eq!(
        sb_detail["attack_if_other_security_status_dropping"],
        json!(false)
    );
    assert_eq!(sb_detail["attack_if_at_war"], json!(true));
    assert_eq!(sb_detail["fuel_bay_view"], json!("corporation_member"));
    assert_eq!(
        sb_detail["fuel_bay_take"],
        json!("starbase_fuel_technician_role")
    );
    assert_eq!(
        sb_detail["anchor"],
        json!("config_starbase_equipment_role")
    );
    assert_eq!(
        sb_detail["unanchor"],
        json!("config_starbase_equipment_role")
    );
    assert_eq!(
        sb_detail["online"],
        json!("config_starbase_equipment_role")
    );
    assert_eq!(
        sb_detail["offline"],
        json!("config_starbase_equipment_role")
    );
    assert_eq!(sb_detail["attack_standing_threshold"], json!(-5.0));
    assert_eq!(sb_detail["attack_security_status_threshold"], json!(-2.5));
    assert_eq!(
        sb_detail["fuels"],
        json!([
            { "type_id": 4_051, "quantity": 1_200 },
            { "type_id": 16_275, "quantity": 500 }
        ])
    );

    // Wrong system_id returns 404; missing system_id returns 400
    assert_eq!(
        check_cached_get(
            &router,
            &spec,
            &EsiHttpRequest::new(
                "GET",
                format!("/v1/corporations/{corp_id}/starbases/{starbase_id}/?system_id=30000142"),
            )
            .with_bearer_token(&token),
            404,
            false,
        )["status"],
        json!(404)
    );
    assert_eq!(
        check_cached_get(
            &router,
            &spec,
            &EsiHttpRequest::new(
                "GET",
                format!("/v1/corporations/{corp_id}/starbases/{starbase_id}/"),
            )
            .with_bearer_token(&token),
            400,
            false,
        )["status"],
        json!(400)
    );
}
