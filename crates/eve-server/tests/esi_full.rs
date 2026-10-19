//! `esi_full` — Integration test suite covering Track 7 ESI endpoints (ESI-5..ESI-7):
//! - `ESI-5`: Industry, Market, Wallet, Contracts routes
//! - `ESI-6`: Fleets, Corporations, Alliances, Mail, Bookmarks routes
//! - `ESI-7`: Incursions, Opportunities, Search, Routes, Planetary Interaction routes
//!
//! Validates OAuth2 Bearer tokens, required scopes, character/corporation ownership,
//! HTTP response headers (ETag, Cache-Control, X-Pages, X-Esi-Error-Limit-*),
//! and validates every payload against the pinned ESI OpenAPI 3.1.0 specification.

#![forbid(unsafe_code)]

use eve_db::{
    AllianceRecord, BookmarkRecord, ContractRecord, CorporationRecord, FleetRecord,
    IndustryJobRecord, InventoryItemRecord, IskCents, MailRecord, PlanetaryColonyRecord,
    WalletJournalEntry,
};
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
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
            "evomu-esi-full-test",
            "https://localhost/callback",
            scopes,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    let bundle = ctx
        .db
        .exchange_pkce_code(&code, "evomu-esi-full-test", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code");
    bundle.access_token
}

fn validate_and_json(
    router: &EsiRouter,
    spec: &OpenApiSpec,
    req: &EsiHttpRequest,
    expected_status: u16,
) -> Value {
    let resp = router.handle_request(req);
    let raw_path = req
        .raw_target
        .split('?')
        .next()
        .unwrap_or(&req.raw_target);
    assert_eq!(
        resp.status_code,
        expected_status,
        "Expected {expected_status} for {} {}, got {} ({})",
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
            "OpenAPI schema validation failed for {} {}: {e}",
            req.method, req.raw_target
        )
    });
    if expected_status == 204 {
        Value::Null
    } else {
        resp.json().expect("valid JSON response")
    }
}

fn setup_esi_test_environment() -> (Arc<ServerContext>, Arc<EsiRouter>, OpenApiSpec) {
    let ctx = Arc::new(ServerContext::new());
    let now = ctx.now_unix();
    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let alliance_id = 99_000_001;
    let fleet_id = 501;
    let contract_id = 4001;
    let contract_item_id = 880_001;
    let job_id = 1001;
    let planet_id = 40_000_001;

    ctx.db.with_mut_snapshot(|snap| {
        // 1. Corporation & Alliance fixtures
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "EvoMu Pioneer Alliance".to_string(),
                short_name: "PIONEER".to_string(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );

        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Deep Space Logistics".to_string(),
                ticker: "DSL".to_string(),
                ceo_id: char_id,
                alliance_id: Some(alliance_id),
                tax_rate: 0.05,
                master_wallet: IskCents::from_isk(250_000_000),
                members: HashSet::from([char_id, 90_000_002]),
            },
        );

        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
            ch.solar_system_id = 30_000_142;
            ch.station_id = Some(60_003_760);
        }

        // 2. Wallet Journal entries
        snap.wallet_journal.push(WalletJournalEntry {
            entry_id: 7001,
            character_id: char_id,
            entry_type: "player_donation".to_string(),
            amount: IskCents::from_isk(50_000_000),
            balance_after: IskCents::from_isk(1_050_000_000),
            reason: "Venture funding subsidy".to_string(),
        });
        snap.wallet_journal.push(WalletJournalEntry {
            entry_id: 7002,
            character_id: char_id,
            entry_type: "market_escrow".to_string(),
            amount: IskCents::from_isk(-10_000_000),
            balance_after: IskCents::from_isk(1_040_000_000),
            reason: "Market purchase order escrow".to_string(),
        });

        // 3. Contract & contract item
        snap.items.insert(
            contract_item_id,
            InventoryItemRecord {
                item_id: contract_item_id,
                type_id: 34, // Tritanium
                owner_id: char_id,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 50_000,
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
                issuer_id: char_id,
                acceptor_id: None,
                contract_type: "ItemExchange".to_string(),
                status: "Outstanding".to_string(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                price: IskCents::from_isk(500_000),
                reward: IskCents(0),
                collateral: IskCents(0),
                item_ids: vec![contract_item_id],
                expiration_unix: now + 86_400,
            },
        );

        // 4. Industry job
        snap.industry_jobs.insert(
            job_id,
            IndustryJobRecord {
                job_id,
                installer_id: char_id,
                facility_id: 60_003_760,
                activity_id: 1, // Manufacturing
                blueprint_item_id: 990_001,
                blueprint_type_id: 688,
                runs: 10,
                product_type_id: 587,
                status: "Active".to_string(),
                duration_seconds: 7200,
                start_unix: now.saturating_sub(1800),
            },
        );

        // 5. Fleet fixture
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: char_id,
                members: vec![char_id],
                wings: vec![1],
                squads: vec![1],
                free_move: false,
                is_registered: false,
            },
        );

        // 6. Planetary Colony fixture
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                owner_id: char_id,
                planet_id,
                solar_system_id: 30_000_142,
                command_center_level: 4,
                pins_count: 6,
                extracted_commodities: HashMap::from([(2268, 500), (2272, 100)]),
                last_extracted_unix: now.saturating_sub(600),
            },
        );

        // 7. Bookmarks fixture
        snap.bookmarks.insert(
            301,
            BookmarkRecord {
                bookmark_id: 301,
                owner_id: char_id,
                label: "Jita 4-4 Undock Safe Spot".to_string(),
                solar_system_id: 30_000_142,
                x: 150_000.0,
                y: -50_000.0,
                z: 80_000.0,
            },
        );

        // 8. Mail fixture
        snap.mails.push(MailRecord {
            message_id: 201,
            sender_id: char_id,
            recipient_id: char_id,
            subject: "Operational Directive Alpha".to_string(),
            body: "Deployment orders confirmed for system 30000142.".to_string(),
        });
    });

    let router = Arc::new(EsiRouter::new(Arc::clone(&ctx)));
    let spec = OpenApiSpec::pinned().clone();
    (ctx, router, spec)
}

#[tokio::test]
async fn test_esi5_industry_market_wallet_contracts_routes() {
    // parity: plan:ESI-5
    // Industry, Market, Wallet, Contracts routes:
    // - /v1/industry/facilities/
    // - /v1/industry/systems/
    // - /v1/characters/{id}/industry/jobs/
    // - /v1/markets/{region_id}/orders/
    // - /v1/markets/{region_id}/history/
    // - /v1/markets/prices/
    // - /v1/characters/{id}/wallet/
    // - /v1/characters/{id}/wallet/journal/
    // - /v1/characters/{id}/wallet/transactions/
    // - /v1/characters/{id}/contracts/
    // - /v1/characters/{id}/contracts/{contract_id}/items/
    // - /v1/contracts/public/{region_id}/

    let (ctx, router, spec) = setup_esi_test_environment();
    let char_id = 90_000_001;
    let contract_id = 4001;

    let full_token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-industry.read_character_jobs.v1".to_string(),
            "esi-wallet.read_character_wallet.v1".to_string(),
            "esi-contracts.read_character_contracts.v1".to_string(),
        ],
    );

    // 1. GET /v1/industry/facilities/ (public)
    let req = EsiHttpRequest::new("GET", "/v1/industry/facilities/");
    let fac_json = validate_and_json(&router, &spec, &req, 200);
    assert!(fac_json.is_array());
    let fac_arr = fac_json.as_array().unwrap();
    assert!(!fac_arr.is_empty(), "facilities must not be empty");
    assert!(fac_arr[0]["facility_id"].is_i64());
    assert!(fac_arr[0]["solar_system_id"].is_i64());

    // 2. GET /v1/industry/systems/ (public)
    let req = EsiHttpRequest::new("GET", "/v1/industry/systems/");
    let ind_sys_json = validate_and_json(&router, &spec, &req, 200);
    assert!(ind_sys_json.is_array());
    let ind_sys_arr = ind_sys_json.as_array().unwrap();
    assert!(!ind_sys_arr.is_empty(), "industry systems must not be empty");
    let jita_sys = ind_sys_arr
        .iter()
        .find(|s| s["solar_system_id"] == json!(30_000_142))
        .expect("Jita industry system");
    assert!(jita_sys["cost_indices"].is_array());

    // 3. GET /v1/characters/{id}/industry/jobs/ (OAuth2 scope: esi-industry.read_character_jobs.v1)
    // 3a. Unauthorized without token -> 401
    let req = EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/industry/jobs/"));
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, 401);

    // 3b. Forbidden with wrong scope -> 403
    let wallet_only_token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec!["esi-wallet.read_character_wallet.v1".to_string()],
    );
    let req = EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/industry/jobs/"))
        .with_bearer_token(&wallet_only_token);
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, 403);

    // 3c. Success with full token -> 200
    let req = EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/industry/jobs/"))
        .with_bearer_token(&full_token);
    let jobs_json = validate_and_json(&router, &spec, &req, 200);
    assert!(jobs_json.is_array());
    let jobs_arr = jobs_json.as_array().unwrap();
    assert!(!jobs_arr.is_empty());
    assert_eq!(jobs_arr[0]["job_id"], json!(1001));
    assert_eq!(jobs_arr[0]["activity_id"], json!(1));
    assert_eq!(jobs_arr[0]["status"], json!("active"));

    // 4. GET /v1/markets/{region_id}/orders/ (public)
    let req = EsiHttpRequest::new("GET", "/v1/markets/10000002/orders/?order_type=all");
    let orders_json = validate_and_json(&router, &spec, &req, 200);
    assert!(orders_json.is_array());

    // 5. GET /v1/markets/{region_id}/history/ (public)
    let req = EsiHttpRequest::new("GET", "/v1/markets/10000002/history/?type_id=34");
    let hist_json = validate_and_json(&router, &spec, &req, 200);
    assert!(hist_json.is_array());
    let hist_arr = hist_json.as_array().unwrap();
    assert!(!hist_arr.is_empty());
    assert!(hist_arr[0]["average"].is_f64() || hist_arr[0]["average"].is_i64());

    // 6. GET /v1/markets/prices/ (public)
    let req = EsiHttpRequest::new("GET", "/v1/markets/prices/");
    let prices_json = validate_and_json(&router, &spec, &req, 200);
    assert!(prices_json.is_array());
    let prices_arr = prices_json.as_array().unwrap();
    assert!(!prices_arr.is_empty());
    assert!(prices_arr.iter().any(|p| p["type_id"] == json!(34)));

    // 7. GET /v1/characters/{id}/wallet/ (OAuth2 scope: esi-wallet.read_character_wallet.v1)
    let req = EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/wallet/"))
        .with_bearer_token(&full_token);
    let wallet_json = validate_and_json(&router, &spec, &req, 200);
    assert!(wallet_json.is_f64());
    assert!(wallet_json.as_f64().unwrap() >= 1_000_000_000.0);

    // 8. GET /v1/characters/{id}/wallet/journal/ (OAuth2 scope: esi-wallet.read_character_wallet.v1)
    let req = EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/wallet/journal/"))
        .with_bearer_token(&full_token);
    let journal_json = validate_and_json(&router, &spec, &req, 200);
    assert!(journal_json.is_array());
    let journal_arr = journal_json.as_array().unwrap();
    assert!(!journal_arr.is_empty());
    assert_eq!(journal_arr[0]["id"], json!(7002));
    assert_eq!(journal_arr[0]["ref_type"], json!("market_escrow"));

    // 9. GET /v1/characters/{id}/wallet/transactions/ (OAuth2 scope: esi-wallet.read_character_wallet.v1)
    let req = EsiHttpRequest::new(
        "GET",
        format!("/v1/characters/{char_id}/wallet/transactions/"),
    )
    .with_bearer_token(&full_token);
    let tx_json = validate_and_json(&router, &spec, &req, 200);
    assert!(tx_json.is_array());
    let tx_arr = tx_json.as_array().unwrap();
    assert!(!tx_arr.is_empty());
    assert!(tx_arr[0]["transaction_id"].is_i64());
    assert!(tx_arr[0]["type_id"].is_i64());

    // 10. GET /v1/characters/{id}/contracts/ (OAuth2 scope: esi-contracts.read_character_contracts.v1)
    let req = EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/contracts/"))
        .with_bearer_token(&full_token);
    let contracts_json = validate_and_json(&router, &spec, &req, 200);
    assert!(contracts_json.is_array());
    let contracts_arr = contracts_json.as_array().unwrap();
    assert!(!contracts_arr.is_empty());
    assert_eq!(contracts_arr[0]["contract_id"], json!(contract_id));
    assert_eq!(contracts_arr[0]["type"], json!("item_exchange"));
    assert_eq!(contracts_arr[0]["status"], json!("outstanding"));

    // 11. GET /v1/characters/{id}/contracts/{contract_id}/items/ (OAuth2 scope: esi-contracts.read_character_contracts.v1)
    let req = EsiHttpRequest::new(
        "GET",
        format!("/v1/characters/{char_id}/contracts/{contract_id}/items/"),
    )
    .with_bearer_token(&full_token);
    let items_json = validate_and_json(&router, &spec, &req, 200);
    assert!(items_json.is_array());
    let items_arr = items_json.as_array().unwrap();
    assert!(!items_arr.is_empty());
    assert_eq!(items_arr[0]["record_id"], json!(880_001));
    assert_eq!(items_arr[0]["type_id"], json!(34));
    assert_eq!(items_arr[0]["quantity"], json!(50_000));
    assert_eq!(items_arr[0]["is_included"], json!(true));

    // 12. GET /v1/contracts/public/{region_id}/ (public)
    let req = EsiHttpRequest::new("GET", "/v1/contracts/public/10000002/");
    let pub_contracts_json = validate_and_json(&router, &spec, &req, 200);
    assert!(pub_contracts_json.is_array());
    let pub_arr = pub_contracts_json.as_array().unwrap();
    assert!(!pub_arr.is_empty());
    assert_eq!(pub_arr[0]["contract_id"], json!(contract_id));
}

#[tokio::test]
async fn test_esi6_fleets_corporations_alliances_mail_bookmarks_routes() {
    // parity: plan:ESI-6
    // Fleets, Corporations, Alliances, Mail, Bookmarks routes:
    // - /v1/fleets/{fleet_id}/
    // - /v1/fleets/{fleet_id}/members/
    // - /v1/fleets/{fleet_id}/wings/
    // - /v1/corporations/{id}/
    // - /v1/corporations/{id}/members/
    // - /v1/corporations/{id}/roles/
    // - /v1/alliances/{id}/
    // - /v1/alliances/{id}/corporations/
    // - /v1/characters/{id}/mail/
    // - /v1/characters/{id}/mail/{mail_id}/
    // - /v1/characters/{id}/bookmarks/

    let (ctx, router, spec) = setup_esi_test_environment();
    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let alliance_id = 99_000_001;
    let fleet_id = 501;

    let full_token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-fleets.read_fleet.v1".to_string(),
            "esi-corporations.read_corporation_membership.v1".to_string(),
            "esi-mail.read_mail.v1".to_string(),
            "esi-bookmarks.read_character_bookmarks.v1".to_string(),
        ],
    );

    // 1. GET /v1/fleets/{fleet_id}/ (OAuth2 scope: esi-fleets.read_fleet.v1)
    // 1a. Unauthorized without token -> 401
    let req = EsiHttpRequest::new("GET", format!("/v1/fleets/{fleet_id}/"));
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, 401);

    // 1b. Success with token -> 200
    let req = EsiHttpRequest::new("GET", format!("/v1/fleets/{fleet_id}/"))
        .with_bearer_token(&full_token);
    let fleet_json = validate_and_json(&router, &spec, &req, 200);
    assert!(fleet_json["motd"].is_string());
    assert_eq!(fleet_json["is_free_move"], json!(false));
    assert_eq!(fleet_json["is_registered"], json!(false));
    assert_eq!(fleet_json["is_voice_enabled"], json!(false));

    // 2. GET /v1/fleets/{fleet_id}/members/ (OAuth2 scope: esi-fleets.read_fleet.v1)
    let req = EsiHttpRequest::new("GET", format!("/v1/fleets/{fleet_id}/members/"))
        .with_bearer_token(&full_token);
    let members_json = validate_and_json(&router, &spec, &req, 200);
    assert!(members_json.is_array());
    let members_arr = members_json.as_array().unwrap();
    assert_eq!(members_arr.len(), 1);
    assert_eq!(members_arr[0]["character_id"], json!(char_id));
    assert_eq!(members_arr[0]["role"], json!("fleet_commander"));

    // 3. GET /v1/fleets/{fleet_id}/wings/ (OAuth2 scope: esi-fleets.read_fleet.v1)
    let req = EsiHttpRequest::new("GET", format!("/v1/fleets/{fleet_id}/wings/"))
        .with_bearer_token(&full_token);
    let wings_json = validate_and_json(&router, &spec, &req, 200);
    assert!(wings_json.is_array());
    let wings_arr = wings_json.as_array().unwrap();
    assert!(!wings_arr.is_empty());
    assert!(wings_arr[0]["id"].is_i64());
    assert!(wings_arr[0]["squads"].is_array());

    // 4. GET /v1/corporations/{id}/ (public)
    let req = EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/"));
    let corp_json = validate_and_json(&router, &spec, &req, 200);
    assert_eq!(corp_json["name"], json!("Deep Space Logistics"));
    assert_eq!(corp_json["ticker"], json!("DSL"));
    assert_eq!(corp_json["ceo_id"], json!(char_id));
    assert_eq!(corp_json["member_count"], json!(2));

    // 5. GET /v1/corporations/{id}/members/ (OAuth2 scope: esi-corporations.read_corporation_membership.v1, CEO guard)
    let req = EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/members/"))
        .with_bearer_token(&full_token);
    let corp_members_json = validate_and_json(&router, &spec, &req, 200);
    assert!(corp_members_json.is_array());
    let corp_members_arr = corp_members_json.as_array().unwrap();
    assert_eq!(corp_members_arr.len(), 2);
    assert!(corp_members_arr.contains(&json!(char_id)));

    // 6. GET /v1/corporations/{id}/roles/ (OAuth2 scope: esi-corporations.read_corporation_membership.v1, CEO guard)
    let req = EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/roles/"))
        .with_bearer_token(&full_token);
    let corp_roles_json = validate_and_json(&router, &spec, &req, 200);
    assert!(corp_roles_json.is_array());
    let corp_roles_arr = corp_roles_json.as_array().unwrap();
    assert!(!corp_roles_arr.is_empty());
    assert_eq!(corp_roles_arr[0]["character_id"], json!(char_id));

    // 7. GET /v1/alliances/{id}/ (public)
    let req = EsiHttpRequest::new("GET", format!("/v1/alliances/{alliance_id}/"));
    let alliance_json = validate_and_json(&router, &spec, &req, 200);
    assert_eq!(alliance_json["name"], json!("EvoMu Pioneer Alliance"));
    assert_eq!(alliance_json["ticker"], json!("PIONEER"));
    assert_eq!(alliance_json["executor_corporation_id"], json!(corp_id));

    // 8. GET /v1/alliances/{id}/corporations/ (public)
    let req = EsiHttpRequest::new("GET", format!("/v1/alliances/{alliance_id}/corporations/"));
    let alliance_corps_json = validate_and_json(&router, &spec, &req, 200);
    assert!(alliance_corps_json.is_array());
    let alliance_corps_arr = alliance_corps_json.as_array().unwrap();
    assert!(alliance_corps_arr.contains(&json!(corp_id)));

    // 9. GET /v1/characters/{id}/mail/ (OAuth2 scope: esi-mail.read_mail.v1)
    let req = EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/mail/"))
        .with_bearer_token(&full_token);
    let mail_json = validate_and_json(&router, &spec, &req, 200);
    assert!(mail_json.is_array());
    let mail_arr = mail_json.as_array().unwrap();
    assert!(!mail_arr.is_empty());
    assert_eq!(mail_arr[0]["mail_id"], json!(201));
    assert_eq!(mail_arr[0]["subject"], json!("Operational Directive Alpha"));

    // 10. GET /v1/characters/{id}/mail/{mail_id}/ (OAuth2 scope: esi-mail.read_mail.v1)
    let req = EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/mail/201/"))
        .with_bearer_token(&full_token);
    let single_mail_json = validate_and_json(&router, &spec, &req, 200);
    assert_eq!(
        single_mail_json["subject"],
        json!("Operational Directive Alpha")
    );
    assert_eq!(
        single_mail_json["body"],
        json!("Deployment orders confirmed for system 30000142.")
    );

    // 11. GET /v1/characters/{id}/bookmarks/ (OAuth2 scope: esi-bookmarks.read_character_bookmarks.v1)
    let req = EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/bookmarks/"))
        .with_bearer_token(&full_token);
    let bms_json = validate_and_json(&router, &spec, &req, 200);
    assert!(bms_json.is_array());
    let bms_arr = bms_json.as_array().unwrap();
    assert!(!bms_arr.is_empty());
    assert_eq!(bms_arr[0]["bookmark_id"], json!(301));
    assert_eq!(bms_arr[0]["label"], json!("Jita 4-4 Undock Safe Spot"));
    assert_eq!(bms_arr[0]["location_id"], json!(30_000_142));
}

#[tokio::test]
async fn test_esi7_incursions_opportunities_search_routes_planetary_interaction() {
    // parity: plan:ESI-7
    // Incursions, Opportunities, Search, Routes, Planetary Interaction:
    // - /v1/incursions/
    // - /v1/opportunities/tasks/
    // - /v2/characters/{id}/search/
    // - /v1/route/{origin}/{destination}/
    // - /v1/characters/{id}/planets/
    // - /v1/characters/{id}/planets/{planet_id}/

    let (ctx, router, spec) = setup_esi_test_environment();
    let char_id = 90_000_001;
    let planet_id = 40_000_001;

    let full_token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-search.search_structures.v1".to_string(),
            "esi-planets.manage_planets.v1".to_string(),
        ],
    );

    // 1. GET /v1/incursions/ (public)
    let req = EsiHttpRequest::new("GET", "/v1/incursions/");
    let incur_json = validate_and_json(&router, &spec, &req, 200);
    assert!(incur_json.is_array());
    let incur_arr = incur_json.as_array().unwrap();
    assert!(!incur_arr.is_empty());
    assert_eq!(incur_arr[0]["state"], json!("established"));
    assert_eq!(incur_arr[0]["type"], json!("Incursion"));
    assert!(incur_arr[0]["influence"].is_f64());
    assert_eq!(incur_arr[0]["staging_solar_system_id"], json!(30_000_142));

    // 2. GET /v1/opportunities/tasks/ (public legacy)
    let req = EsiHttpRequest::new("GET", "/v1/opportunities/tasks/");
    let tasks_json = validate_and_json(&router, &spec, &req, 200);
    assert!(tasks_json.is_array());
    let tasks_arr = tasks_json.as_array().unwrap();
    assert!(!tasks_arr.is_empty());

    // 3. GET /v2/characters/{id}/search/ (OAuth2 scope: esi-search.search_structures.v1)
    let req = EsiHttpRequest::new(
        "GET",
        format!("/v2/characters/{char_id}/search/?categories=solar_system,inventory_type&search=Jita"),
    )
    .with_bearer_token(&full_token);
    let search_json = validate_and_json(&router, &spec, &req, 200);
    assert!(search_json["solar_system"].is_array());
    let sys_matches = search_json["solar_system"].as_array().unwrap();
    assert!(sys_matches.contains(&json!(30_000_142)));

    // 4. GET /v1/route/{origin}/{destination}/ (public)
    let req = EsiHttpRequest::new("GET", "/v1/route/30000142/30002187/");
    let route_json = validate_and_json(&router, &spec, &req, 200);
    assert!(route_json.is_array());
    let route_arr = route_json.as_array().unwrap();
    assert!(!route_arr.is_empty());
    assert_eq!(route_arr[0], json!(30_000_142));
    assert_eq!(*route_arr.last().unwrap(), json!(30_002_187));

    // 5. GET /v1/characters/{id}/planets/ (OAuth2 scope: esi-planets.manage_planets.v1)
    // 5a. Unauthorized without token -> 401
    let req = EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/planets/"));
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, 401);

    // 5b. Success with token -> 200
    let req = EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/planets/"))
        .with_bearer_token(&full_token);
    let planets_json = validate_and_json(&router, &spec, &req, 200);
    assert!(planets_json.is_array());
    let planets_arr = planets_json.as_array().unwrap();
    assert!(!planets_arr.is_empty());
    assert_eq!(planets_arr[0]["planet_id"], json!(planet_id));
    assert_eq!(planets_arr[0]["solar_system_id"], json!(30_000_142));
    assert_eq!(planets_arr[0]["owner_id"], json!(char_id));
    assert_eq!(planets_arr[0]["upgrade_level"], json!(4));
    assert_eq!(planets_arr[0]["num_pins"], json!(6));

    // 6. GET /v1/characters/{id}/planets/{planet_id}/ (OAuth2 scope: esi-planets.manage_planets.v1)
    let req = EsiHttpRequest::new(
        "GET",
        format!("/v1/characters/{char_id}/planets/{planet_id}/"),
    )
    .with_bearer_token(&full_token);
    let colony_detail_json = validate_and_json(&router, &spec, &req, 200);
    assert!(colony_detail_json["pins"].is_array());
    assert!(colony_detail_json["links"].is_array());
    assert!(colony_detail_json["routes"].is_array());
    let pins = colony_detail_json["pins"].as_array().unwrap();
    assert_eq!(pins.len(), 2);
    assert!(pins[0]["latitude"].is_f64());
    assert!(pins[0]["longitude"].is_f64());
    assert!(pins[0]["contents"].is_array());
}
