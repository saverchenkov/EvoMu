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
    crate::common::pin_clock(&ctx);
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
                member_roles: HashMap::from([(char_id, eve_db::corp_roles::ROLE_DIRECTOR)]),
                applications: HashMap::new(),
                wallet_divisions: Vec::new(),
                description: String::new(),
                url: String::new(),
                hq_station_id: None,
            },
        );

        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
            ch.solar_system_id = 30_000_142;
            ch.station_id = Some(60_003_760);
        }

        // 2. Wallet Journal & Transaction entries
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
        snap.wallet_journal_timestamps.insert(7001, now.saturating_sub(7200));
        snap.wallet_journal_timestamps.insert(7002, now.saturating_sub(3600));
        snap.char_wallet_transactions.insert(
            char_id,
            vec![eve_db::WalletTransactionRecord {
                transaction_id: 8001,
                date_unix: now.saturating_sub(3600),
                location_id: 60_003_760,
                type_id: 34,
                unit_price: 5.5,
                quantity: 1000,
                client_id: 90_000_002,
                is_buy: true,
                is_personal: true,
                journal_ref_id: 7002,
            }],
        );

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
                issuer_corp_id: corp_id,
                assignee_id: 0,
                availability: "Public".to_string(),
                buyout: IskCents(0),
                current_bid: IskCents(0),
                current_bidder_id: None,
                requested_items: Vec::new(),
                bid_history: Vec::new(),
                date_issued_unix: now,
                title: "Test Contract".to_string(),
                for_corp: false,
                ..Default::default()
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
                start_unix: now.saturating_sub(1800), ..Default::default() },
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
                wing_details: HashMap::new(),
                squad_details: HashMap::new(),
                member_details: HashMap::new(),
                invites: HashSet::new(),
                motd: "".to_string(),
            },
        );

        // 6. Planetary Colony & Incursion fixtures
        snap.incursions.insert(
            20_000_020,
            eve_db::IncursionRecord {
                constellation_id: 20_000_020,
                staging_solar_system_id: 30_000_142,
                infested_solar_systems: vec![30_000_142, 30_000_144],
                faction_id: 500_019,
                has_boss: true,
                influence: 0.9,
                state: "established".to_string(),
                incursion_type: "Incursion".to_string(),
            },
        );
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
        snap.planetary_colony_layouts.insert(
            (char_id, planet_id),
            eve_db::PlanetaryColonyLayoutRecord {
                pins: vec![
                    eve_db::PlanetaryPinRecord {
                        pin_id: planet_id * 10 + 1,
                        type_id: 2254,
                        latitude: 0.5,
                        longitude: 1.0,
                        schematic_id: None,
                        install_time_unix: None,
                        expiry_time_unix: None,
                        last_cycle_start_unix: None,
                        contents: vec![
                            eve_db::PlanetaryPinContentRecord {
                                type_id: 2268,
                                amount: 500,
                            },
                            eve_db::PlanetaryPinContentRecord {
                                type_id: 2272,
                                amount: 100,
                            },
                        ],
                    },
                    eve_db::PlanetaryPinRecord {
                        pin_id: planet_id * 10 + 2,
                        type_id: 2848,
                        latitude: 0.6,
                        longitude: 1.1,
                        schematic_id: Some(65),
                        install_time_unix: Some(now.saturating_sub(3600)),
                        expiry_time_unix: Some(now + 86_400),
                        last_cycle_start_unix: Some(now.saturating_sub(600)),
                        contents: Vec::new(),
                    },
                ],
                links: vec![eve_db::PlanetaryLinkRecord {
                    source_pin_id: planet_id * 10 + 2,
                    destination_pin_id: planet_id * 10 + 1,
                    link_level: 1,
                }],
                routes: vec![eve_db::PlanetaryRouteRecord {
                    route_id: 1,
                    source_pin_id: planet_id * 10 + 2,
                    destination_pin_id: planet_id * 10 + 1,
                    content_type_id: 2393,
                    quantity: 20,
                    waypoints: Vec::new(),
                }],
            },
        );
        snap.market_history.insert(
            (10_000_002, 34),
            vec![eve_db::MarketHistoryDayRecord {
                date: "2025-12-15".to_string(),
                average: 5.2,
                highest: 5.5,
                lowest: 4.9,
                order_count: 14,
                volume: 120_000,
                open_price: Some(5.0),
                close_price: Some(5.3),
            }],
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
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
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

    // 7. GET /v2/alliances/{id}/icons/ (public)
    let req = EsiHttpRequest::new("GET", format!("/v2/alliances/{alliance_id}/icons/"));
    let alliance_icons_json = validate_and_json(&router, &spec, &req, 200);
    assert!(alliance_icons_json["px64x64"]
        .as_str()
        .unwrap_or_default()
        .contains(&alliance_id.to_string()));

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


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod esi_b2_ext {
    use super::*;

// Live HTTP + MachoNet RPC + SQLite persistence integration tests for 12 ESI routes.


use crate::common;
use crate::esi_market_industry_pi::{issue_token_with_scopes, make_item, make_structure};
use eve_db::{
    corp_roles::{ROLE_ACCOUNTANT, ROLE_DIRECTOR, ROLE_STATION_MANAGER, ROLE_TRADER},
    CorporationRecord, InventoryFlag, IskCents, TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{send_raw_esi_request, EsiHttpRequest, EsiServer, OpenApiSpec};
use eve_server::router::ServerContext;
use glam::DVec3;
use serde_json::Value;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SYSTEM_ID: u32 = 30_000_142;
const JITA_REGION_ID: u32 = 10_000_002;
const JITA_PLANET_ID: i64 = 40_009_082;
const TRITANIUM_TYPE_ID: u32 = 34;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> PyRep {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
        .unwrap_or_else(|e| panic!("RPC {service}.{method} failed: {e:?}"))
}

async fn http_get_json(
    addr: SocketAddr,
    spec: &OpenApiSpec,
    path: &str,
    token: Option<&str>,
) -> Value {
    let mut req = EsiHttpRequest::new("GET", path);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let resp = send_raw_esi_request(addr, &req)
        .await
        .expect("send_raw_esi_request");
    assert_eq!(
        resp.status_code,
        200,
        "Expected 200 for {path}, got {} ({})",
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
    let raw_path = path.split('?').next().unwrap_or(path);
    spec.validate_response("GET", raw_path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failed for {path}: {e}"));
    assert!(resp.header("ETag").is_some(), "200 response for {path} must include ETag");
    assert!(
        resp.header("Expires").is_some() || resp.header("Cache-Control").is_some(),
        "200 response for {path} must include cache headers"
    );
    resp.json().expect("JSON body")
}

async fn http_expect_status(
    addr: SocketAddr,
    path: &str,
    token: Option<&str>,
    expected: u16,
) {
    let mut req = EsiHttpRequest::new("GET", path);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let resp = send_raw_esi_request(addr, &req)
        .await
        .expect("send_raw_esi_request");
    assert_eq!(
        resp.status_code,
        expected,
        "Expected {expected} for {path}, got {} ({})",
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
}

#[tokio::test]
async fn test_esi_b2_corp_mining_and_customs_offices_live_http_and_sqlite() {
    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);

    let ceo_id = 90_000_001_i64;
    let miner_id = 90_000_002_i64;
    let accountant_id = 90_000_003_i64;
    let director_id = 90_000_004_i64;
    let corp_id = 98_000_811_i64;
    let athanor_id = 1_000_000_811_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "B2 Mining & Customs Corp".into(),
            ticker: "B2MC".into(),
            ceo_id,
            ..Default::default()
        };
        corp.member_roles.insert(ceo_id, ROLE_DIRECTOR);
        corp.member_roles.insert(miner_id, ROLE_STATION_MANAGER);
        corp.member_roles.insert(accountant_id, ROLE_ACCOUNTANT);
        corp.member_roles.insert(director_id, ROLE_DIRECTOR);
        snap.corporations.insert(corp_id, corp);
        for cid in [ceo_id, miner_id] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.corporation_id = corp_id;
            }
        }
        let template_char = snap.characters.get(&ceo_id).unwrap().clone();
        for (cid, name) in [(accountant_id, "B2 Accountant"), (director_id, "B2 Non-CEO Director")] {
            let mut ch = template_char.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.name = name.into();
            ch.corporation_id = corp_id;
            snap.characters.insert(cid, ch);
        }
        snap.structures.insert(
            athanor_id,
            make_structure(athanor_id, 35835, corp_id, JITA_SYSTEM_ID, "B2 Moon Athanor"),
        );
    });

    let mut ceo_sess = setup_pilot(&ctx, 11, "demo", "demo-secret", ceo_id).await;
    let mut miner_sess = setup_pilot(&ctx, 12, "other", "other-secret", miner_id).await;

    // 1. Start moon extraction via moonExtractions.StartNewExtraction
    let start_res = rpc(&ctx, &mut ceo_sess, "moonExtractions", "StartNewExtraction", &[PyRep::Integer(athanor_id), PyRep::Integer(864_000)]).await;
    assert_eq!(start_res.dict_get("status").and_then(|v| v.as_str()), Some("Started"));

    // 2. Advance time, fracture chunk, and mine via real mining laser cycle
    ctx.advance_time_seconds(864_000);
    let frac_res = rpc(&ctx, &mut ceo_sess, "moonExtractions", "FractureChunkWithStructure", &[PyRep::Integer(athanor_id)]).await;
    let PyRep::List(ast_ids) = frac_res.dict_get("asteroidIDs").expect("asteroidIDs") else {
        panic!("Expected asteroidIDs list");
    };
    let first_ast_id = ast_ids[0].as_i64().unwrap();

    let undock = rpc(&ctx, &mut miner_sess, "stationSvc", "Undock", &[]).await;
    let ship_id = undock.dict_get("shipID").and_then(|v| v.as_i64()).unwrap();
    let jita = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);
    let miner_mod_id = 800_000_811_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 32880;
        }
        snap.skills.entry(miner_id).or_default().insert(
            3386,
            TrainedSkillRecord { type_id: 3386, level: 3, skill_points: 50_000.0 },
        );
        snap.items.retain(|_, item| item.location_id != ship_id);
        snap.items.insert(
            miner_mod_id,
            make_item(miner_mod_id, 483, miner_id, ship_id, InventoryFlag::HiSlot0, 1, true),
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);
    let ast_pos = jita.get_ball_snapshot(first_ast_id).await.unwrap().unwrap().1.position;
    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await {
        sb.position = ast_pos + DVec3::new(500.0, 0.0, 0.0);
        let _ = jita.spawn_ball(sb).await;
    }
    let mine_res = crate::mining_cycle_engine::mine_once(&ctx, &mut miner_sess, miner_mod_id, first_ast_id)
        .await
        .expect("mine_once");
    assert!(mine_res.units_delivered > 0);

    // 3. Anchor, online, and configure corporation customs office via real client RPCs (no GM role)
    let lowsec_sys = 30_000_192_u32;
    let lowsec_planet = ctx
        .sde
        .celestials_in_system(lowsec_sys)
        .into_iter()
        .find(|c| c.group_id == 7)
        .expect("lowsec planet");
    let lowsec_planet_id = lowsec_planet.celestial_id;
    let ceo_undock = rpc(&ctx, &mut ceo_sess, "stationSvc", "Undock", &[]).await;
    let ceo_ship_id = ceo_undock.dict_get("shipID").and_then(|v| v.as_i64()).unwrap();
    let mut ceo_ball = jita.get_ball_snapshot(ceo_ship_id).await.unwrap().unwrap().1;
    let _ = jita.remove_ball(ceo_ship_id).await;
    let (gantry_id, poco_item_id) = (800_000_813_i64, 800_000_814_i64);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.station_id = None;
            ch.solar_system_id = lowsec_sys;
            ch.standings.insert(500_026, 2.0);
        }
        if let Some(ship) = snap.items.get_mut(&ceo_ship_id) {
            ship.location_id = i64::from(lowsec_sys);
        }
        snap.items.insert(gantry_id, make_item(gantry_id, 3962, corp_id, ceo_ship_id, InventoryFlag::Cargo, 1, false));
        snap.items.insert(poco_item_id, make_item(poco_item_id, 2233, corp_id, ceo_ship_id, InventoryFlag::Cargo, 1, false));
    });
    ceo_sess.station_id = None;
    ceo_sess.solar_system_id = Some(lowsec_sys);
    let kw = HashMap::new();
    let lowsec = ctx.systems.get_or_spawn(lowsec_sys);
    assert_eq!(
        ctx.dispatch_rpc(&mut ceo_sess, "posMgr", None, "AnchorOrbital", &[PyRep::Integer(gantry_id), PyRep::Integer(lowsec_planet_id)], &kw, None).await.unwrap_err().code,
        "ShipNotInSpace"
    );
    let near_planet = lowsec_planet.position + DVec3::new(lowsec_planet.radius + 5_000.0, 0.0, 0.0);
    ceo_ball.position = lowsec_planet.position + DVec3::new(lowsec_planet.radius + 250_000_000.0, 0.0, 0.0);
    lowsec.spawn_ball(ceo_ball.clone()).await.expect("spawn far ball");
    assert_eq!(
        ctx.dispatch_rpc(&mut ceo_sess, "posMgr", None, "AnchorOrbital", &[PyRep::Integer(gantry_id), PyRep::Integer(lowsec_planet_id)], &kw, None).await.unwrap_err().code,
        "OrbitalOutOfRange"
    );
    lowsec.set_ball_position(ceo_ship_id, near_planet).await;
    let anchor_res = rpc(&ctx, &mut ceo_sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_id), PyRep::Integer(lowsec_planet_id)]).await;
    let orbital_id = anchor_res.dict_get("orbitalID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(orbital_id, gantry_id);

    let _ = lowsec.remove_ball(ceo_ship_id).await;
    assert_eq!(
        ctx.dispatch_rpc(&mut ceo_sess, "posMgr", None, "OnlineOrbital", &[PyRep::Integer(orbital_id), PyRep::Integer(poco_item_id)], &kw, None).await.unwrap_err().code,
        "ShipNotInSpace"
    );
    ceo_ball.position = near_planet + DVec3::new(25_000.0, 0.0, 0.0);
    lowsec.spawn_ball(ceo_ball).await.expect("spawn far online ball");
    assert_eq!(
        ctx.dispatch_rpc(&mut ceo_sess, "posMgr", None, "OnlineOrbital", &[PyRep::Integer(orbital_id), PyRep::Integer(poco_item_id)], &kw, None).await.unwrap_err().code,
        "OrbitalOutOfRange"
    );
    lowsec.set_ball_position(ceo_ship_id, near_planet).await;
    let _ = rpc(&ctx, &mut ceo_sess, "posMgr", "OnlineOrbital", &[PyRep::Integer(orbital_id), PyRep::Integer(poco_item_id)]).await;
    let tax_kv = PyRep::keyval([
        ("corporation", PyRep::Real(0.02)),
        ("alliance", PyRep::Real(0.05)),
        ("standingHigh", PyRep::Real(0.05)),
        ("standingGood", PyRep::Real(0.08)),
        ("standingNeutral", PyRep::Real(0.12)),
    ]);
    assert_eq!(
        ctx.dispatch_rpc(&mut ceo_sess, "planetOrbitalRegistryBroker", None, "UpdateSettings", &[PyRep::Integer(999_999_999)], &kw, None).await.unwrap_err().code,
        "CrpAccessDenied"
    );
    let _ = rpc(&ctx, &mut ceo_sess, "planetOrbitalRegistryBroker", "UpdateSettings", &[
        PyRep::Integer(orbital_id), PyRep::Integer(18), tax_kv, PyRep::Real(1.0), PyRep::Bool(true), PyRep::Bool(true), PyRep::None,
    ]).await;
    let info_rep = rpc(&ctx, &mut ceo_sess, "planetOrbitalRegistryBroker", "GetSettingsInfo", &[PyRep::Integer(orbital_id)]).await;
    let PyRep::Tuple(info_t) = &info_rep else { panic!("GetSettingsInfo 6-tuple") };
    assert_eq!(info_t.len(), 6);
    assert!(info_t[0].as_i64().is_some() && matches!(info_t[1], PyRep::KeyVal(_)) && info_t[2].as_f64() == Some(1.0));
    assert_eq!((info_t[3].as_bool(), info_t[4].as_bool(), &info_t[5]), (Some(true), Some(true), &PyRep::None));

    let (addr, _router, handle) = EsiServer::spawn(Arc::clone(&ctx), "127.0.0.1:0").await.expect("spawn ESI");
    let spec = OpenApiSpec::pinned();
    let scopes = vec!["esi-industry.read_corporation_mining.v1", "esi-planets.read_customs_offices.v1"];
    let tok_station_mgr = issue_token_with_scopes(&ctx, miner_id, scopes.clone());
    let tok_accountant = issue_token_with_scopes(&ctx, accountant_id, scopes.clone());
    let tok_non_ceo_director = issue_token_with_scopes(&ctx, director_id, scopes.clone());
    let tok_director = issue_token_with_scopes(&ctx, ceo_id, scopes);

    let ext_path = format!("/v1/corporation/{corp_id}/mining/extractions/");
    let obs_path = format!("/v1/corporation/{corp_id}/mining/observers/");
    let poco_path = format!("/v1/corporations/{corp_id}/customs_offices/");

    // Auth / role / 404 checks
    http_expect_status(addr, &ext_path, None, 401).await;
    http_expect_status(addr, &obs_path, Some(&tok_station_mgr), 403).await;
    http_expect_status(addr, &poco_path, Some(&tok_accountant), 403).await;
    http_expect_status(addr, "/v1/corporation/999999999/mining/extractions/", Some(&tok_director), 403).await;

    // Station Manager succeeds on extractions; non-CEO Director succeeds on customs_offices
    let exts = http_get_json(addr, &spec, &ext_path, Some(&tok_station_mgr)).await;
    assert_eq!(exts.as_array().unwrap().len(), 1);
    assert_eq!(exts[0]["structure_id"], athanor_id);

    http_expect_status(addr, &poco_path, Some(&tok_station_mgr), 403).await;
    let pocos = http_get_json(addr, &spec, &poco_path, Some(&tok_non_ceo_director)).await;
    assert_eq!(pocos.as_array().unwrap().len(), 1);
    assert_eq!(pocos[0]["office_id"], orbital_id);
    assert_eq!(pocos[0]["system_id"], lowsec_sys);

    // Accountant (non-CEO, ROLE_ACCOUNTANT only) & non-CEO Director succeed on mining observers
    let obs = http_get_json(addr, &spec, &obs_path, Some(&tok_accountant)).await;
    let obs_id = obs[0]["observer_id"].as_i64().unwrap();
    let obs_detail_path = format!("/v1/corporation/{corp_id}/mining/observers/{obs_id}/");
    http_expect_status(addr, &obs_detail_path, Some(&tok_station_mgr), 403).await;
    http_expect_status(addr, &format!("/v1/corporation/{corp_id}/mining/observers/999999999/"), Some(&tok_accountant), 404).await;
    let entries = http_get_json(addr, &spec, &obs_detail_path, Some(&tok_accountant)).await;
    assert_eq!(entries[0]["character_id"], miner_id);
    assert_eq!(entries[0]["quantity"], mine_res.units_delivered);
    let entries_dir = http_get_json(addr, &spec, &obs_detail_path, Some(&tok_non_ceo_director)).await;
    assert_eq!(entries_dir[0]["quantity"], mine_res.units_delivered);

    handle.abort();

    // Reload from SQLite and verify persistence over live HTTP
    let ctx2 = Arc::new(crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "esi_b2_corp_mining_poco"));
    let (addr2, _router2, handle2) = EsiServer::spawn(Arc::clone(&ctx2), "127.0.0.1:0").await.expect("spawn ESI 2");
    let tok_dir2 = issue_token_with_scopes(&ctx2, ceo_id, vec!["esi-industry.read_corporation_mining.v1", "esi-planets.read_customs_offices.v1"]);
    let exts2 = http_get_json(addr2, &spec, &ext_path, Some(&tok_dir2)).await;
    assert_eq!(exts2.as_array().unwrap().len(), 1);
    let pocos2 = http_get_json(addr2, &spec, &poco_path, Some(&tok_dir2)).await;
    assert_eq!(pocos2.as_array().unwrap().len(), 1);
    let entries2 = http_get_json(addr2, &spec, &obs_detail_path, Some(&tok_dir2)).await;
    assert_eq!(entries2[0]["quantity"], mine_res.units_delivered);

    handle2.abort();
}

#[tokio::test]
async fn test_esi_b2_character_planets_and_orders_live_http_and_sqlite() {
    // parity: esi:GET /characters/{character_id}/orders, esi:GET /characters/{character_id}/orders/history
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/orders",
        "esi:GET /characters/{character_id}/orders/history",
    ]);

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);

    let char_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let mut s1 = setup_pilot(&ctx, 21, "demo", "demo-secret", char_id).await;

    // 1. Deploy Command Center + Storage Facility + Link via planetMgr.UserUpdateNetwork
    let undock = rpc(&ctx, &mut s1, "stationSvc", "Undock", &[]).await;
    let ship_id = undock.dict_get("shipID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            2495,
            TrainedSkillRecord { type_id: 2495, level: 4, skill_points: 100_000.0 },
        );
        snap.items.insert(
            800_000_821,
            make_item(800_000_821, 2534, char_id, ship_id, InventoryFlag::Cargo, 1, false),
        );
    });
    let _ = rpc(&ctx, &mut s1, "planetMgr", "UserUpdateNetwork", &[
        PyRep::Integer(JITA_PLANET_ID),
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(2534), PyRep::Real(0.1), PyRep::Real(0.2)]),
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(2541), PyRep::Real(0.15), PyRep::Real(0.25)]),
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(1)]),
        ]),
    ]).await;

    // Dock back at Jita 4-4 and place two character orders, cancelling one via marketProxy
    s1.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(JITA_STATION_ID);
        }
    });
    let sell_open = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                ("price", PyRep::Real(12.0)),
                ("quantity", PyRep::Integer(200)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await;
    let open_oid = sell_open.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();

    let sell_cancel = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                ("price", PyRep::Real(14.0)),
                ("quantity", PyRep::Integer(150)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await;
    let cancel_oid = sell_cancel.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    let _ = rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(cancel_oid)]).await;

    let (addr, _router, handle) = EsiServer::spawn(Arc::clone(&ctx), "127.0.0.1:0").await.expect("spawn ESI");
    let spec = OpenApiSpec::pinned();
    let scopes = vec!["esi-planets.manage_planets.v1", "esi-markets.read_character_orders.v1"];
    let tok_owner = issue_token_with_scopes(&ctx, char_id, scopes.clone());
    let tok_other = issue_token_with_scopes(&ctx, other_id, scopes);

    let planets_path = format!("/v1/characters/{char_id}/planets/");
    let planet_detail_path = format!("/v1/characters/{char_id}/planets/{JITA_PLANET_ID}/");
    let orders_path = format!("/v1/characters/{char_id}/orders/");
    let history_path = format!("/v1/characters/{char_id}/orders/history/");

    http_expect_status(addr, &planets_path, None, 401).await;
    http_expect_status(addr, &planets_path, Some(&tok_other), 403).await;
    http_expect_status(addr, &format!("/v1/characters/{char_id}/planets/40000001/"), Some(&tok_owner), 404).await;

    let planets = http_get_json(addr, &spec, &planets_path, Some(&tok_owner)).await;
    assert_eq!(planets.as_array().unwrap().len(), 1);
    assert_eq!(planets[0]["planet_id"], JITA_PLANET_ID);
    assert_eq!(planets[0]["num_pins"], 2);

    let layout = http_get_json(addr, &spec, &planet_detail_path, Some(&tok_owner)).await;
    assert_eq!(layout["pins"].as_array().unwrap().len(), 2);
    assert_eq!(layout["links"].as_array().unwrap().len(), 1);

    let orders = http_get_json(addr, &spec, &orders_path, Some(&tok_owner)).await;
    assert!(orders.as_array().unwrap().iter().any(|o| o["order_id"] == open_oid));

    let hist = http_get_json(addr, &spec, &history_path, Some(&tok_owner)).await;
    let cancelled_row = hist
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["order_id"] == cancel_oid)
        .expect("cancelled order in history");
    assert_eq!(cancelled_row["state"], "cancelled");

    handle.abort();

    let ctx2 = Arc::new(crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "esi_b2_char_pi_orders"));
    let (addr2, _router2, handle2) = EsiServer::spawn(Arc::clone(&ctx2), "127.0.0.1:0").await.expect("spawn ESI 2");
    let tok_owner2 = issue_token_with_scopes(
        &ctx2,
        char_id,
        vec![
            "esi-planets.manage_planets.v1",
            "esi-markets.read_character_orders.v1",
        ],
    );
    let layout2 = http_get_json(addr2, &spec, &planet_detail_path, Some(&tok_owner2)).await;
    assert_eq!(layout2["pins"].as_array().unwrap().len(), 2);
    let hist2 = http_get_json(addr2, &spec, &history_path, Some(&tok_owner2)).await;
    assert!(hist2.as_array().unwrap().iter().any(|o| o["order_id"] == cancel_oid));

    handle2.abort();
}

#[tokio::test]
async fn test_esi_b2_market_history_structure_and_corp_orders_live_http_and_sqlite() {
    // parity: esi:GET /markets/structures/{structure_id}, esi:GET /corporations/{corporation_id}/orders, esi:GET /corporations/{corporation_id}/orders/history
    eve_testkit::attest(&[
        "esi:GET /markets/structures/{structure_id}",
        "esi:GET /corporations/{corporation_id}/orders",
        "esi:GET /corporations/{corporation_id}/orders/history",
    ]);

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);

    let ceo_id = 90_000_001_i64;
    let trader_id = 90_000_002_i64;
    let corp_id = 98_000_831_i64;
    let fortizar_id = 1_000_000_831_001_i64;
    let bare_astrahus_id = 1_000_000_831_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "B2 Market & Citadel Corp".into(),
            ticker: "B2MC".into(),
            ceo_id,
            master_wallet: IskCents(50_000_000_00),
            ..Default::default()
        };
        corp.member_roles.insert(ceo_id, ROLE_DIRECTOR);
        corp.member_roles.insert(trader_id, ROLE_TRADER);
        snap.corporations.insert(corp_id, corp);
        for cid in [ceo_id, trader_id] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.corporation_id = corp_id;
                ch.balance = IskCents(50_000_000_00);
            }
        }
        snap.structures.insert(
            fortizar_id,
            make_structure(fortizar_id, 35833, corp_id, JITA_SYSTEM_ID, "B2 Market Fortizar"),
        );
        snap.structures.insert(
            bare_astrahus_id,
            make_structure(bare_astrahus_id, 35832, corp_id, JITA_SYSTEM_ID, "B2 Bare Astrahus"),
        );
        // Fit online Standup Market Hub I (35892) on fortizar_id
        snap.items.insert(
            800_000_831,
            make_item(800_000_831, 35892, corp_id, fortizar_id, InventoryFlag::ServiceSlot0, 1, true),
        );
    });

    let mut s1 = setup_pilot(&ctx, 31, "demo", "demo-secret", ceo_id).await;
    let mut s2 = setup_pilot(&ctx, 32, "other", "other-secret", trader_id).await;

    // 1. Execute real trade at Jita 4-4 to generate server-aggregated OHLCV market_history
    let _ = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                ("price", PyRep::Real(6.0)),
                ("quantity", PyRep::Integer(100)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await;
    let _ = rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(6.0),
            PyRep::Integer(50),
            PyRep::Integer(90),
            PyRep::Integer(1),
        ],
    )
    .await;

    // 2. Place structure market order on fortizar_id
    s1.structure_id = Some(fortizar_id);
    s1.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.station_id = None;
            ch.structure_id = Some(fortizar_id);
        }
    });
    let _ = rpc(&ctx, &mut s1, "marketProxy", "PlaceBuyOrder", &[
        PyRep::Integer(fortizar_id), PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Real(5.5),
        PyRep::Integer(100), PyRep::Integer(90), PyRep::Integer(1),
    ]).await;

    // 3. Place two corporation orders (one active, one cancelled) via useCorp=true
    s2.station_id = Some(JITA_STATION_ID);
    let mut corp_kw = HashMap::new();
    corp_kw.insert("useCorp".to_string(), PyRep::Bool(true));
    corp_kw.insert("accountKey".to_string(), PyRep::Integer(1000));
    let corp_open = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(4.5),
                PyRep::Integer(200),
                PyRep::Integer(90),
                PyRep::Integer(1),
            ],
            &corp_kw,
            None,
        )
        .await
        .expect("corp open order");
    let corp_open_oid = corp_open.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();

    let corp_cancel = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(4.2),
                PyRep::Integer(100),
                PyRep::Integer(90),
                PyRep::Integer(1),
            ],
            &corp_kw,
            None,
        )
        .await
        .expect("corp cancel order");
    let corp_cancel_oid = corp_cancel.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    let _ = rpc(&ctx, &mut s2, "marketProxy", "CancelCharOrder", &[PyRep::Integer(corp_cancel_oid)]).await;

    let (addr, _router, handle) = EsiServer::spawn(Arc::clone(&ctx), "127.0.0.1:0").await.expect("spawn ESI");
    let spec = OpenApiSpec::pinned();
    let tok_trader = issue_token_with_scopes(&ctx, trader_id, vec!["esi-markets.structure_markets.v1", "esi-markets.read_corporation_orders.v1"]);

    // Public market history
    http_expect_status(addr, &format!("/v1/markets/{JITA_REGION_ID}/history/"), None, 400).await;
    let hist = http_get_json(addr, &spec, &format!("/v1/markets/{JITA_REGION_ID}/history/?type_id={TRITANIUM_TYPE_ID}"), None,).await;
    assert!(hist[0]["volume"].as_i64().unwrap() >= 50);
    assert!(hist[0]["average"].as_f64().unwrap() > 0.0);

    // Structure market: 403 on bare Astrahus, 200 on Fortizar with online Market Hub
    http_expect_status(addr, &format!("/v1/markets/structures/{bare_astrahus_id}/"), Some(&tok_trader), 403).await;
    let struct_orders = http_get_json(addr, &spec, &format!("/v1/markets/structures/{fortizar_id}/"), Some(&tok_trader),).await;
    assert_eq!(struct_orders.as_array().unwrap().len(), 1);
    assert_eq!(struct_orders[0]["location_id"], fortizar_id);

    // Corporation orders & history accessible by Accountant | Trader (with Director/CEO always allowed)
    let corp_orders = http_get_json(addr, &spec, &format!("/v1/corporations/{corp_id}/orders/"), Some(&tok_trader),).await;
    assert!(corp_orders.as_array().unwrap().iter().any(|o| o["order_id"] == corp_open_oid));

    let corp_hist = http_get_json(addr, &spec, &format!("/v1/corporations/{corp_id}/orders/history/"), Some(&tok_trader),).await;
    assert!(corp_hist.as_array().unwrap().iter().any(|o| o["order_id"] == corp_cancel_oid && o["state"] == "cancelled"));

    handle.abort();

    let ctx2 = Arc::new(crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "esi_b2_market_struct_corp"));
    let (addr2, _router2, handle2) = EsiServer::spawn(Arc::clone(&ctx2), "127.0.0.1:0").await.expect("spawn ESI 2");
    let tok_trader2 = issue_token_with_scopes(&ctx2, trader_id, vec!["esi-markets.structure_markets.v1", "esi-markets.read_corporation_orders.v1"]);
    let hist2 = http_get_json(addr2, &spec, &format!("/v1/markets/{JITA_REGION_ID}/history/?type_id={TRITANIUM_TYPE_ID}"), None,).await;
    assert!(hist2[0]["volume"].as_i64().unwrap() >= 50);
    let corp_orders2 = http_get_json(addr2, &spec, &format!("/v1/corporations/{corp_id}/orders/"), Some(&tok_trader2),).await;
    assert!(corp_orders2.as_array().unwrap().iter().any(|o| o["order_id"] == corp_open_oid));

    handle2.abort();
}

}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod esi_b6_ext {
    use super::*;

// Live ESI HTTP integration tests for Batch 6 parity closure:
// - Faction Warfare (`/fw/stats`, `/fw/systems`, `/fw/leaderboards`, `/fw/leaderboards/characters`,
//   `/fw/leaderboards/corporations`, `/fw/wars`, `/characters/{character_id}/fw/stats`,
//   `/corporations/{corporation_id}/fw/stats`)
// - Incursions & Sovereignty (`/incursions`, `/sovereignty/campaigns`, `/sovereignty/structures`)
// - Loyalty Store Offers (`/loyalty/stores/{corporation_id}/offers`)

use std::collections::HashMap;
use std::sync::Arc;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::process_ship_destruction_with_aggression;
use eve_server::services::pve_incursion_sites::on_incursion_boss_destroyed;
use eve_server::services::pve_special_sites::{ensure_special_sites_spawned, with_pve_special_reg};
use eve_server::services::sov_services::{
    claim_system_sovereignty, record_hub_hack, TYPE_SOVEREIGNTY_HUB,
};
use serde_json::Value;

use crate::common;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token_with_scopes(
    ctx: &ServerContext,
    uname: &str,
    character_id: i64,
    scopes: Vec<String>,
) -> String {
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            uname,
            "secret",
            character_id,
            "evomu-esi-b6-test",
            "https://localhost/callback",
            scopes,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-esi-b6-test", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

fn validate_and_json(
    router: &EsiRouter,
    spec: &OpenApiSpec,
    expected_status: u16,
    req: &EsiHttpRequest,
) -> (Value, HashMap<String, String>) {
    let resp = router.handle_request(req);
    assert_eq!(resp.status_code, expected_status, "path={}", req.raw_target);
    let raw_path = req.raw_target.split('?').next().unwrap_or(&req.raw_target);
    spec.validate_response(&req.method, raw_path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failed for {}: {e}", req.raw_target));
    let json_val = if resp.status_code == 304 || resp.body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&resp.body).expect("Valid JSON response body")
    };
    (json_val, resp.headers)
}

async fn create_pilot_in_new_corp(
    ctx: &ServerContext,
    uname: &str,
    cname: &str,
    ticker: &str,
) -> (i64, i64, SessionState) {
    let uid = ctx.db.provision_player_account(uname, "secret");
    let char_id = ctx
        .db
        .create_character(uid, cname, 1, 1, 11, 17, &ctx.sde)
        .expect("create_character")
        .character_id;
    let mut sess = SessionState::new(char_id as u64);
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String(uname.into()), PyRep::String("secret".into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = eve_db::IskCents::from_isk(10_000_000);
        }
        snap.skills.entry(char_id).or_default().insert(
            3363,
            eve_db::TrainedSkillRecord {
                type_id: 3363,
                level: 1,
                skill_points: 250.0,
            },
        );
    });
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        ctx.now_unix() + 15,
    )));
    let corp_res = ctx
        .dispatch_rpc(
            &mut sess,
            "corpRegistry",
            None,
            "AddCorporation",
            &[
                PyRep::String(format!("{cname} Corp").into()),
                PyRep::String(ticker.into()),
                PyRep::String("Test Corp".into()),
                PyRep::String("".into()),
                PyRep::Real(0.05),
                PyRep::Bool(false),
                PyRep::Integer(0),
                PyRep::Integer(0),
            ],
            &kw,
            None,
        )
        .await
        .expect("AddCorporation");
    let corp_id = match corp_res {
        PyRep::Integer(id) => id,
        _ => sess.corporation_id.unwrap_or(0),
    };
    (char_id, corp_id, sess)
}

#[tokio::test]
async fn test_esi_b6_fw_stats_systems_leaderboards_and_wars_live_rpc_driven() {
    // parity: esi:GET /fw/wars
    eve_testkit::attest(&[
        "esi:GET /fw/wars",
    ]);

    let db_path = std::env::temp_dir().join(format!("esi_b6_fw_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let ctx = Arc::new(ServerContext::from_sqlite(&db_path).expect("sqlite ctx"));
    common::pin_clock(&ctx);

    let (char_a, corp_a, mut sess_a) =
        create_pilot_in_new_corp(&ctx, "fw_pilot_a", "FW Pilot A", "FWPA").await;
    let (char_b, _corp_b, mut sess_b) =
        create_pilot_in_new_corp(&ctx, "fw_pilot_b", "FW Pilot B", "FWPB").await;
    let (char_u, corp_u, _sess_u) =
        create_pilot_in_new_corp(&ctx, "fw_unenlisted", "Unenlisted Pilot", "UNEN").await;

    let kw = HashMap::new();
    // Enlist Pilot A's corp in Caldari State (500001) and Pilot B in Gallente Federation (500004)
    ctx.dispatch_rpc(
        &mut sess_a,
        "facWarMgr",
        None,
        "JoinFactionAsCorporation",
        &[PyRep::Integer(500_001)],
        &kw,
        None,
    )
    .await
    .expect("JoinFactionAsCorporation");
    ctx.dispatch_rpc(
        &mut sess_b,
        "facWarMgr",
        None,
        "JoinFactionAsCharacter",
        &[PyRep::Integer(500_004)],
        &kw,
        None,
    )
    .await
    .expect("JoinFactionAsCharacter");

    // Pick a Gallente-occupied FW warzone system and donate LP as Caldari to advance victory points
    let fw_sys_id = ctx.db.with_snapshot(|snap| {
        snap.fw_system_occupations
            .values()
            .find(|o| o.occupier_faction_id == 500_004)
            .map(|o| o.solar_system_id)
            .expect("Gallente FW system")
    });
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_b) {
            ch.loyalty_points.insert(1_000_181, 5_000);
            ch.solar_system_id = fw_sys_id;
        }
    });
    sess_b.solar_system_id = Some(fw_sys_id);
    ctx.dispatch_rpc(
        &mut sess_b,
        "facWarMgr",
        None,
        "DonateLPsToSolarSystem",
        &[PyRep::Integer(fw_sys_id as i64), PyRep::Integer(1_000)],
        &kw,
        None,
    )
    .await
    .expect("DonateLPsToSolarSystem");
    eve_server::services::fw_services::add_victory_points(&ctx, fw_sys_id, 500);

    // Record a live PvP killmail between opposing FW pilots -> combat hook populates FW kills, VP, and LP
    process_ship_destruction_with_aggression(
        &ctx,
        char_b,
        880_000_001,
        587,
        char_a,
        2873,
        fw_sys_id,
        glam::DVec3::new(100.0, 200.0, 300.0),
        false,
    );

    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    // 1. /fw/wars
    let (wars, _) = validate_and_json(&router, &spec, 200, &EsiHttpRequest::new("GET", "/v1/fw/wars/"));
    let wars_arr = wars.as_array().expect("wars array");
    assert!(
        wars_arr.iter().any(|w| w["faction_id"] == 500_001 && w["against_id"] == 500_004),
        "Caldari vs Gallente war pair must be present"
    );

    // 2. /fw/systems
    let (systems, _) = validate_and_json(&router, &spec, 200, &EsiHttpRequest::new("GET", "/v1/fw/systems/"));
    let sys_entry = systems
        .as_array()
        .expect("systems array")
        .iter()
        .find(|s| s["solar_system_id"] == fw_sys_id as i64)
        .expect("fw system entry");
    assert_eq!(sys_entry["occupier_faction_id"], 500_004);
    assert!(sys_entry["victory_points"].as_i64().unwrap_or(0) > 0);
    assert_eq!(sys_entry["contested"], "contested");

    // 3. /fw/stats
    let (stats, _) = validate_and_json(&router, &spec, 200, &EsiHttpRequest::new("GET", "/v1/fw/stats/"));
    let cal_stats = stats
        .as_array()
        .expect("stats array")
        .iter()
        .find(|s| s["faction_id"] == 500_001)
        .expect("Caldari stats");
    assert!(cal_stats["pilots"].as_i64().unwrap_or(0) >= 1);
    assert!(cal_stats["kills"]["total"].as_i64().unwrap_or(0) >= 1);

    // 4. /fw/leaderboards, /fw/leaderboards/characters, /fw/leaderboards/corporations
    let (lb, _) = validate_and_json(&router, &spec, 200, &EsiHttpRequest::new("GET", "/v1/fw/leaderboards/"));
    assert!(lb["kills"]["active_total"].is_array());
    let (lb_chars, _) = validate_and_json(
        &router,
        &spec,
        200,
        &EsiHttpRequest::new("GET", "/v1/fw/leaderboards/characters/"),
    );
    assert!(lb_chars["kills"]["active_total"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["character_id"] == char_a));
    let (lb_corps, _) = validate_and_json(
        &router,
        &spec,
        200,
        &EsiHttpRequest::new("GET", "/v1/fw/leaderboards/corporations/"),
    );
    assert!(lb_corps["kills"]["active_total"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["corporation_id"] == corp_a));

    // 5. /characters/{character_id}/fw/stats: 401, 403, 200, 304, 404
    let char_token = issue_token_with_scopes(
        &ctx,
        "fw_pilot_a",
        char_a,
        vec![
            "esi-characters.read_fw_stats.v1".to_string(),
            "esi-corporations.read_fw_stats.v1".to_string(),
        ],
    );
    let no_scope_token = issue_token_with_scopes(&ctx, "fw_pilot_a", char_a, vec![]);

    let char_path = format!("/v1/characters/{char_a}/fw/stats/");
    validate_and_json(&router, &spec, 401, &EsiHttpRequest::new("GET", &char_path));
    validate_and_json(
        &router,
        &spec,
        403,
        &EsiHttpRequest::new("GET", &char_path).with_bearer_token(&no_scope_token),
    );
    let (char_fw, headers) = validate_and_json(
        &router,
        &spec,
        200,
        &EsiHttpRequest::new("GET", &char_path).with_bearer_token(&char_token),
    );
    assert_eq!(char_fw["faction_id"], 500_001);
    assert!(char_fw["enlisted_on"].is_string());
    assert!(char_fw["kills"]["total"].as_i64().unwrap_or(0) >= 1);

    let etag = headers.get("ETag").or_else(|| headers.get("etag")).cloned().expect("ETag");
    let mut cached_req = EsiHttpRequest::new("GET", &char_path).with_bearer_token(&char_token);
    cached_req.headers.insert("if-none-match".to_string(), etag);
    validate_and_json(&router, &spec, 304, &cached_req);

    validate_and_json(
        &router,
        &spec,
        403,
        &EsiHttpRequest::new("GET", "/v1/characters/999999999/fw/stats/").with_bearer_token(&char_token),
    );
    let unenlisted_token = issue_token_with_scopes(
        &ctx,
        "fw_unenlisted",
        char_u,
        vec![
            "esi-characters.read_fw_stats.v1".to_string(),
            "esi-corporations.read_fw_stats.v1".to_string(),
        ],
    );
    let (unenlisted_char_fw, _) = validate_and_json(
        &router,
        &spec,
        200,
        &EsiHttpRequest::new("GET", &format!("/v1/characters/{char_u}/fw/stats/"))
            .with_bearer_token(&unenlisted_token),
    );
    assert!(unenlisted_char_fw.get("faction_id").is_none());
    assert!(unenlisted_char_fw.get("enlisted_on").is_none());
    assert_eq!(unenlisted_char_fw["kills"]["total"], 0);
    assert_eq!(unenlisted_char_fw["victory_points"]["total"], 0);

    // 6. /corporations/{corporation_id}/fw/stats: 401, 403, 200, 404
    let corp_path = format!("/v1/corporations/{corp_a}/fw/stats/");
    validate_and_json(&router, &spec, 401, &EsiHttpRequest::new("GET", &corp_path));
    validate_and_json(
        &router,
        &spec,
        403,
        &EsiHttpRequest::new("GET", &corp_path).with_bearer_token(&no_scope_token),
    );
    let (corp_fw, _) = validate_and_json(
        &router,
        &spec,
        200,
        &EsiHttpRequest::new("GET", &corp_path).with_bearer_token(&char_token),
    );
    assert_eq!(corp_fw["faction_id"], 500_001);
    assert!(corp_fw["pilots"].as_i64().unwrap_or(0) >= 1);
    assert!(corp_fw["enlisted_on"].is_string());

    validate_and_json(
        &router,
        &spec,
        403,
        &EsiHttpRequest::new("GET", "/v1/corporations/999999999/fw/stats/").with_bearer_token(&char_token),
    );
    let (unenlisted_corp_fw, _) = validate_and_json(
        &router,
        &spec,
        200,
        &EsiHttpRequest::new("GET", &format!("/v1/corporations/{corp_u}/fw/stats/"))
            .with_bearer_token(&unenlisted_token),
    );
    assert!(unenlisted_corp_fw.get("faction_id").is_none());
    assert!(unenlisted_corp_fw.get("enlisted_on").is_none());
    assert_eq!(unenlisted_corp_fw["pilots"], 0);
    assert_eq!(unenlisted_corp_fw["kills"]["total"], 0);
    assert_eq!(unenlisted_corp_fw["victory_points"]["total"], 0);

    // 7. SQLite persistence across ServerContext reload
    drop(router);
    drop(ctx);
    let ctx_reloaded = Arc::new(ServerContext::from_sqlite(&db_path).expect("reload sqlite"));
    let reloaded_token = issue_token_with_scopes(
        &ctx_reloaded,
        "fw_pilot_a",
        char_a,
        vec!["esi-characters.read_fw_stats.v1".to_string()],
    );
    let router_reloaded = EsiRouter::new(ctx_reloaded);
    let (char_fw_after, _) = validate_and_json(
        &router_reloaded,
        &spec,
        200,
        &EsiHttpRequest::new("GET", &char_path).with_bearer_token(&reloaded_token),
    );
    assert_eq!(char_fw_after["faction_id"], 500_001);
    assert!(char_fw_after["kills"]["total"].as_i64().unwrap_or(0) >= 1);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_esi_b6_incursions_and_sovereignty_campaigns_structures_live_state() {
    // parity: esi:GET /sovereignty/structures
    eve_testkit::attest(&[
        "esi:GET /sovereignty/structures",
    ]);

    let db_path = std::env::temp_dir().join(format!("esi_b6_sov_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let ctx = Arc::new(ServerContext::from_sqlite(&db_path).expect("sqlite ctx"));
    common::pin_clock(&ctx);

    // 1. Seed live SDE-topology incursions via ensure_special_sites_spawned
    ensure_special_sites_spawned(&ctx).await;
    let (cid, staging_sys, site_id, initial_inf) = with_pve_special_reg(&ctx, |reg| {
        let (&cid, inc) = reg.incursions.iter().next().expect("seeded incursion");
        let site = inc.sites.iter().find(|s| !s.locked_until_low_influence).expect("unlocked site");
        (cid, site.solar_system_id, site.site_id.clone(), inc.influence)
    });

    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let (inc_list, _) = validate_and_json(&router, &spec, 200, &EsiHttpRequest::new("GET", "/v1/incursions/"));
    let inc_arr = inc_list.as_array().expect("incursions array");
    assert_eq!(inc_arr.len(), 3);
    let target_inc = inc_arr
        .iter()
        .find(|i| i["constellation_id"] == cid as i64)
        .expect("target incursion");
    assert_eq!(target_inc["faction_id"], 500_019);
    assert_eq!(target_inc["type"], "Incursion");

    // Complete an incursion site and verify influence decreases via ESI
    let (pilot_id, _, _) = create_pilot_in_new_corp(&ctx, "inc_pilot", "Inc Pilot", "INCP").await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&pilot_id) {
            ch.solar_system_id = staging_sys;
        }
    });
    let boss_id = common::clear_incursion_site_escort_waves(&ctx, staging_sys, &site_id).await;
    let _ = ctx.systems.get_or_spawn(staging_sys).remove_ball(boss_id).await;
    on_incursion_boss_destroyed(&ctx, staging_sys, boss_id, Some(pilot_id)).await;

    let (inc_after, _) = validate_and_json(&router, &spec, 200, &EsiHttpRequest::new("GET", "/v1/incursions/"));
    let after_inf = inc_after
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["constellation_id"] == cid as i64)
        .map(|i| i["influence"].as_f64().unwrap())
        .unwrap_or(0.0);
    assert!(after_inf < initial_inf, "Incursion influence must decrease after site completion");

    // 2. Anchor a Sovereignty Hub in nullsec, claim sovereignty, and hack it to start a campaign
    let (_char_s, corp_s, _) =
        create_pilot_in_new_corp(&ctx, "sov_pilot", "Sov Pilot", "SOVP").await;
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= -0.1)
        .map(|s| s.solar_system_id)
        .expect("nullsec system");
    let alliance_id = 99_000_601_i64;
    let hub_id = 77_000_601_i64;
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default();
        if let Some(c) = snap.corporations.get_mut(&corp_s) {
            c.alliance_id = Some(alliance_id);
        }
        snap.structures.insert(
            hub_id,
            eve_db::UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: corp_s,
                solar_system_id: null_sys,
                name: "B6 Sov Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.structure_vulnerability_windows.insert(hub_id, (now + 3600, now + 7200));
    });
    claim_system_sovereignty(&ctx, null_sys, alliance_id, hub_id, corp_s)
        .expect("claim_system_sovereignty");

    let (sov_structs, _) = validate_and_json(
        &router,
        &spec,
        200,
        &EsiHttpRequest::new("GET", "/v1/sovereignty/structures/"),
    );
    let hub_entry = sov_structs
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["structure_id"] == hub_id)
        .expect("sov hub in /sovereignty/structures");
    assert_eq!(hub_entry["alliance_id"], alliance_id);
    assert_eq!(hub_entry["solar_system_id"], null_sys as i64);

    // Hack the hub via live sov_services::record_hub_hack -> creates campaign & scores
    record_hub_hack(&ctx, null_sys, hub_id, 3600);
    let (campaigns, _) = validate_and_json(
        &router,
        &spec,
        200,
        &EsiHttpRequest::new("GET", "/v1/sovereignty/campaigns/"),
    );
    let camp = campaigns
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["structure_id"] == hub_id)
        .expect("campaign in /sovereignty/campaigns");
    assert_eq!(camp["event_type"], "ihub_defense");
    assert_eq!(camp["defender_id"], alliance_id);
    assert_eq!(camp["attackers_score"], 0.35);
    assert_eq!(camp["defender_score"], 0.65);

    // 3. SQLite persistence across reload
    drop(router);
    drop(ctx);
    let ctx_reloaded = Arc::new(ServerContext::from_sqlite(&db_path).expect("reload sqlite"));
    let router_reloaded = EsiRouter::new(ctx_reloaded);
    let (campaigns_after, _) = validate_and_json(
        &router_reloaded,
        &spec,
        200,
        &EsiHttpRequest::new("GET", "/v1/sovereignty/campaigns/"),
    );
    assert!(campaigns_after
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["structure_id"] == hub_id));
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_esi_b6_loyalty_store_offers_sde_and_rpc_parity() {
    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    // 1. Unseeded Paragon (1000419) & CONCORD (1000125) catalogs
    let (paragon_offers, _) = validate_and_json(
        &router,
        &spec,
        200,
        &EsiHttpRequest::new("GET", "/v1/loyalty/stores/1000419/offers/"),
    );
    let paragon_arr = paragon_offers.as_array().expect("Paragon offers array");
    assert!(!paragon_arr.is_empty(), "Paragon V24 SDE offers must be non-empty");

    let (concord_offers, _) = validate_and_json(
        &router,
        &spec,
        200,
        &EsiHttpRequest::new("GET", "/v1/loyalty/stores/1000125/offers/"),
    );
    assert!(
        concord_offers.as_array().unwrap().len() >= 200,
        "CONCORD catalog must be non-empty"
    );

    // 2. Verify LPStoreMgr.GetAvailableOffersFromCorp RPC parity with ESI
    let (_char_lp, _corp_lp, mut sess) =
        create_pilot_in_new_corp(&ctx, "lp_pilot", "LP Pilot", "LPPP").await;
    let kw = HashMap::new();
    let rpc_res = ctx
        .dispatch_rpc(
            &mut sess,
            "LPStoreMgr",
            None,
            "GetAvailableOffersFromCorp",
            &[PyRep::Integer(1_000_419)],
            &kw,
            None,
        )
        .await
        .expect("LPStoreMgr.GetAvailableOffersFromCorp");
    let PyRep::List(rpc_offers) = rpc_res else {
        panic!("Expected PyRep::List from GetAvailableOffersFromCorp");
    };
    assert_eq!(rpc_offers.len(), paragon_arr.len());

    // 3. 404 on unknown corporation
    validate_and_json(
        &router,
        &spec,
        404,
        &EsiHttpRequest::new("GET", "/v1/loyalty/stores/999999999/offers/"),
    );
}

}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod esi_p6_ext {
    use super::*;

// `ESI-6` live HTTP + RPC + SQLite persistence integration tests for Contracts and Industry routes.


use crate::esi_market_industry_pi::{assert_status, get_json, issue_token_with_scopes, validate_and_json};
use eve_db::{
    corp_roles::{ROLE_CONTRACT_MANAGER, ROLE_DIRECTOR, ROLE_FACTORY_MANAGER},
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord,
    UpwellStructureRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use eve_server::services::industry_pi::industry_facilities_bps::save_facility_taxes;
use std::collections::HashMap;
use std::sync::Arc;

fn temp_sqlite_path(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("evomu_p6_esi1_{tag}_{nanos}.sqlite"))
}

fn open_seeded_sqlite_ctx(tag: &str) -> (std::path::PathBuf, Arc<ServerContext>) {
    let db_path = temp_sqlite_path(tag);
    let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
    let ctx = Arc::new(ServerContext::with_repository(repo));
    let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    ctx.db
        .try_with_mut_snapshot(|snap| {
            *snap = demo;
            Ok::<_, std::convert::Infallible>(())
        })
        .unwrap();
    (db_path, ctx)
}

fn setup_session(ctx: &ServerContext, char_id: i64, corp_id: i64, station_id: i64, roles: u64) -> SessionState {
    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(station_id);
    sess.solar_system_id = Some(30_000_142);
    sess.region_id = Some(10_000_002);
    sess.corp_role = roles;
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
            ch.solar_system_id = 30_000_142;
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(char_id);
            corp.member_roles.insert(char_id, roles);
        }
        Ok(())
    });
    sess
}

fn seed_hangar_item(ctx: &ServerContext, owner_id: i64, station_id: i64, type_id: u32, qty: i32) -> i64 {
    ctx.db
        .try_with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let iid = snap.next_entity_id;
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id,
                    owner_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            Ok::<_, std::convert::Infallible>(iid)
        })
        .unwrap()
}

#[tokio::test]
async fn test_esi_p6_char_and_public_contracts_live_rpc_and_sqlite_persistence() {
    // parity: esi:GET /characters/{character_id}/contracts, esi:GET /characters/{character_id}/contracts/{contract_id}/bids, esi:GET /characters/{character_id}/contracts/{contract_id}/items, esi:GET /contracts/public/{region_id}, esi:GET /contracts/public/bids/{contract_id}, esi:GET /contracts/public/items/{contract_id}
    let (db_path, ctx) = open_seeded_sqlite_ctx("char_public_contracts");
    let router = EsiRouter::new(ctx.clone());
    let spec = OpenApiSpec::pinned();
    let empty = HashMap::new();

    let station_id = 60_003_760_i64;
    let region_id = 10_000_002_i64;
    let (char1, corp1, char2, corp2) = ctx.db.with_snapshot(|snap| {
        let mut chars: Vec<_> = snap.characters.values().collect();
        chars.sort_by_key(|c| c.character_id);
        (
            chars[0].character_id,
            chars[0].corporation_id,
            chars[1].character_id,
            chars[1].corporation_id,
        )
    });

    let mut sess1 = setup_session(&ctx, char1, corp1, station_id, 0);
    let mut sess2 = setup_session(&ctx, char2, corp2, station_id, 0);
    let item_id = seed_hangar_item(&ctx, char1, station_id, 34, 500);

    let c_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(2), // Auction
                PyRep::Integer(0), // Public
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(station_id),
                PyRep::Integer(station_id),
                PyRep::Real(100_000.0),
                PyRep::Real(0.0),
                PyRep::Real(500_000.0),
                PyRep::String("P6 Tritanium Auction".into()),
                PyRep::String("Live RPC auction".into()),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(item_id),
                    PyRep::Integer(500),
                ])]),
            ],
            &empty,
            None,
        )
        .await
        .expect("CreateContract");
    let PyRep::KeyVal(kv) = c_res else {
        panic!("expected KeyVal from CreateContract");
    };
    let cid = kv.get("contractID").unwrap().as_i64().unwrap();

    ctx.dispatch_rpc(
        &mut sess2,
        "contractProxy",
        None,
        "PlaceBid",
        &[PyRep::Integer(cid), PyRep::Real(125_000.0)],
        &empty,
        None,
    )
    .await
    .expect("PlaceBid");

    let tok1 = issue_token_with_scopes(&ctx, char1, vec!["esi-contracts.read_character_contracts.v1"]);
    let tok2 = issue_token_with_scopes(&ctx, char2, vec!["esi-contracts.read_character_contracts.v1"]);
    let bad_scope = issue_token_with_scopes(&ctx, char1, vec!["esi-wallet.read_character_wallet.v1"]);

    // Auth / 401 / 403 / 404 checks
    assert_status(&router, format!("/v1/characters/{char1}/contracts/"), None, 401);
    assert_status(&router, format!("/v1/characters/{char1}/contracts/"), Some(&bad_scope), 403);
    assert_status(&router, format!("/v1/characters/{char1}/contracts/"), Some(&tok2), 403);
    assert_status(&router, "/v1/characters/999999999/contracts/", Some(&tok1), 403);
    assert_status(&router, format!("/v1/characters/{char1}/contracts/999999999/items/"), Some(&tok1), 404);
    assert_status(&router, format!("/v1/characters/{char1}/contracts/999999999/bids/"), Some(&tok1), 404);
    assert_status(&router, "/v1/contracts/public/999999999/", None, 404);

    // Character contracts, items, bids
    let char_contracts = get_json(&router, &spec, format!("/v1/characters/{char1}/contracts/"), Some(&tok1));
    let found = char_contracts
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["contract_id"] == cid)
        .expect("contract in char list");
    assert_eq!(found["type"], "auction");
    assert_eq!(found["status"], "outstanding");
    assert_eq!(found["title"], "P6 Tritanium Auction");

    let items_req = EsiHttpRequest::new("GET", format!("/v1/characters/{char1}/contracts/{cid}/items/"))
        .with_bearer_token(&tok1);
    let items_resp = router.handle_request(&items_req);
    assert_eq!(items_resp.status_code, 200);
    assert!(items_resp.header("X-Pages").is_none());
    let char_items = validate_and_json(&router, &spec, &items_req, 200);
    assert_eq!(char_items.as_array().unwrap().len(), 1);
    assert_eq!(char_items[0]["type_id"], 34);
    assert_eq!(char_items[0]["quantity"], 500);

    let char_bids = get_json(&router, &spec, format!("/v1/characters/{char1}/contracts/{cid}/bids/"), Some(&tok1));
    assert_eq!(char_bids.as_array().unwrap().len(), 1);
    assert_eq!(char_bids[0]["amount"], 125_000.0);
    assert_eq!(char_bids[0]["bidder_id"], char2);

    // Public contracts, items, bids
    let pub_contracts = get_json(&router, &spec, format!("/v1/contracts/public/{region_id}/"), None);
    assert!(pub_contracts.as_array().unwrap().iter().any(|c| c["contract_id"] == cid));

    let pub_items = get_json(&router, &spec, format!("/v1/contracts/public/items/{cid}/"), None);
    assert_eq!(pub_items.as_array().unwrap().len(), 1);
    assert_eq!(pub_items[0]["type_id"], 34);

    let pub_bids = get_json(&router, &spec, format!("/v1/contracts/public/bids/{cid}/"), None);
    assert_eq!(pub_bids.as_array().unwrap().len(), 1);
    assert_eq!(pub_bids[0]["amount"], 125_000.0);

    // SQLite persistence check across repository reopen
    drop(router);
    drop(ctx);
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo2));
    let router2 = EsiRouter::new(ctx2);
    let pub_bids2 = get_json(&router2, &spec, format!("/v1/contracts/public/bids/{cid}/"), None);
    assert_eq!(pub_bids2.as_array().unwrap().len(), 1);
    assert_eq!(pub_bids2[0]["amount"], 125_000.0);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_esi_p6_corp_contracts_live_rpc_auth_and_sqlite_persistence() {
    // parity: esi:GET /corporations/{corporation_id}/contracts, esi:GET /corporations/{corporation_id}/contracts/{contract_id}/bids, esi:GET /corporations/{corporation_id}/contracts/{contract_id}/items
    let (db_path, ctx) = open_seeded_sqlite_ctx("corp_contracts");
    let router = EsiRouter::new(ctx.clone());
    let spec = OpenApiSpec::pinned();
    let empty = HashMap::new();

    let station_id = 60_003_760_i64;
    let corp1 = 98_000_001_i64;
    let member_char = 90_000_777_i64;
    let (char1, char2, corp2) = ctx.db.with_snapshot(|snap| {
        let mut chars: Vec<_> = snap.characters.values().collect();
        chars.sort_by_key(|c| c.character_id);
        (
            chars[0].character_id,
            chars[1].character_id,
            chars[1].corporation_id,
        )
    });
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        let mut plain_member = snap.characters[&char1].clone();
        plain_member.character_id = member_char;
        plain_member.name = "P6 Ordinary Member".to_string();
        plain_member.corporation_id = corp1;
        snap.characters.insert(member_char, plain_member);

        let mut corp = CorporationRecord {
            corporation_id: corp1,
            name: "P6 Corp".to_string(),
            ticker: "P6C".to_string(),
            ceo_id: char1,
            master_wallet: IskCents::from_isk(500_000_000),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        corp.members.insert(member_char);
        corp.member_roles.insert(member_char, 0);
        snap.corporations.insert(corp1, corp);
        Ok(())
    });

    let mut sess1 = setup_session(
        &ctx,
        char1,
        corp1,
        station_id,
        ROLE_CONTRACT_MANAGER | ROLE_DIRECTOR,
    );
    let _sess_member = setup_session(&ctx, member_char, corp1, station_id, 0);
    let mut sess2 = setup_session(&ctx, char2, corp2, station_id, ROLE_DIRECTOR);
    let item_id = seed_hangar_item(&ctx, char1, station_id, 35, 250);
    let personal_item_id = seed_hangar_item(&ctx, char1, station_id, 34, 100);

    let mut kwargs = HashMap::new();
    kwargs.insert("forCorp".to_string(), PyRep::Bool(true));
    let c_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(2), // Auction
                PyRep::Integer(0), // Public
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(station_id),
                PyRep::Integer(station_id),
                PyRep::Real(200_000.0),
                PyRep::Real(0.0),
                PyRep::Real(800_000.0),
                PyRep::String("P6 Corp Pyerite Auction".into()),
                PyRep::String("Corp auction".into()),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(item_id),
                    PyRep::Integer(250),
                ])]),
            ],
            &kwargs,
            None,
        )
        .await
        .expect("CreateContract forCorp");
    let PyRep::KeyVal(kv) = c_res else {
        panic!("expected KeyVal");
    };
    let cid = kv.get("contractID").unwrap().as_i64().unwrap();

    // Also create a personal contract (`forCorp` omitted/false) by `char1` to verify character vs corp isolation.
    let p_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(1), // ItemExchange
                PyRep::Integer(0), // Public
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(station_id),
                PyRep::Integer(station_id),
                PyRep::Real(50_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
                PyRep::String("P6 Personal Tritanium".into()),
                PyRep::String("Personal exchange".into()),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(personal_item_id),
                    PyRep::Integer(100),
                ])]),
            ],
            &empty,
            None,
        )
        .await
        .expect("CreateContract personal");
    let PyRep::KeyVal(p_kv) = p_res else {
        panic!("expected KeyVal");
    };
    let personal_cid = p_kv.get("contractID").unwrap().as_i64().unwrap();

    ctx.dispatch_rpc(
        &mut sess2,
        "contractProxy",
        None,
        "PlaceBid",
        &[PyRep::Integer(cid), PyRep::Real(250_000.0)],
        &empty,
        None,
    )
    .await
    .expect("PlaceBid on corp auction");

    let tok_member = issue_token_with_scopes(&ctx, member_char, vec!["esi-contracts.read_corporation_contracts.v1"]);
    let tok_char1 = issue_token_with_scopes(&ctx, char1, vec!["esi-contracts.read_character_contracts.v1"]);
    let tok_other = issue_token_with_scopes(&ctx, char2, vec!["esi-contracts.read_corporation_contracts.v1"]);

    // Cross-corp and unauthenticated negative tests on all 3 corporation contract endpoints
    assert_status(&router, format!("/v1/corporations/{corp1}/contracts/"), None, 401);
    assert_status(&router, format!("/v1/corporations/{corp1}/contracts/"), Some(&tok_other), 403);
    assert_status(&router, format!("/v1/corporations/{corp1}/contracts/{cid}/items/"), Some(&tok_other), 403);
    assert_status(&router, format!("/v1/corporations/{corp1}/contracts/{cid}/bids/"), Some(&tok_other), 403);
    assert_status(&router, format!("/v1/corporations/{corp1}/contracts/999999999/items/"), Some(&tok_member), 404);
    assert_status(&router, format!("/v1/corporations/{corp1}/contracts/999999999/bids/"), Some(&tok_member), 404);

    // Non-Director ordinary corporation member (`roles == 0`) gets 200 OK on all 3 corp contract routes
    let corp_contracts = get_json(&router, &spec, format!("/v1/corporations/{corp1}/contracts/"), Some(&tok_member));
    let corp_arr = corp_contracts.as_array().unwrap();
    let found = corp_arr
        .iter()
        .find(|c| c["contract_id"] == cid)
        .expect("corp contract in list for ordinary member");
    assert_eq!(found["for_corporation"], true);
    assert_eq!(found["issuer_corporation_id"], corp1);
    assert!(
        !corp_arr.iter().any(|c| c["contract_id"] == personal_cid),
        "personal character contract must not leak into corporation contracts"
    );

    // Character contracts for `char1` must include `personal_cid` and exclude `for_corp == true` `cid`
    let char1_contracts = get_json(&router, &spec, format!("/v1/characters/{char1}/contracts/"), Some(&tok_char1));
    let char1_arr = char1_contracts.as_array().unwrap();
    assert!(char1_arr.iter().any(|c| c["contract_id"] == personal_cid));
    assert!(
        !char1_arr.iter().any(|c| c["contract_id"] == cid),
        "for_corp contract must not appear in issuer character contracts"
    );

    let corp_items = get_json(
        &router,
        &spec,
        format!("/v1/corporations/{corp1}/contracts/{cid}/items/"),
        Some(&tok_member),
    );
    assert_eq!(corp_items.as_array().unwrap().len(), 1);
    assert_eq!(corp_items[0]["type_id"], 35);
    assert_eq!(corp_items[0]["quantity"], 250);

    let corp_bids = get_json(
        &router,
        &spec,
        format!("/v1/corporations/{corp1}/contracts/{cid}/bids/"),
        Some(&tok_member),
    );
    assert_eq!(corp_bids.as_array().unwrap().len(), 1);
    assert_eq!(corp_bids[0]["amount"], 250_000.0);

    // SQLite persistence check
    drop(router);
    drop(ctx);
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo2));
    let router2 = EsiRouter::new(ctx2);
    let tok_member2 = issue_token_with_scopes(&router2.ctx, member_char, vec!["esi-contracts.read_corporation_contracts.v1"]);
    let corp_bids2 = get_json(
        &router2,
        &spec,
        format!("/v1/corporations/{corp1}/contracts/{cid}/bids/"),
        Some(&tok_member2),
    );
    assert_eq!(corp_bids2.as_array().unwrap().len(), 1);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_esi_p6_industry_jobs_and_facilities_live_rpc_and_sqlite_persistence() {
    let (db_path, ctx) = open_seeded_sqlite_ctx("industry_jobs_facilities");
    let router = EsiRouter::new(ctx.clone());
    let spec = OpenApiSpec::pinned();
    let empty = HashMap::new();

    let station_id = 60_003_760_i64;
    let corp1 = 98_000_001_i64;
    let raitaru_id = 1_030_000_991_001_i64;
    let bare_astrahus_id = 1_030_000_991_002_i64;
    let (char1, char2, corp2) = ctx.db.with_snapshot(|snap| {
        let mut chars: Vec<_> = snap.characters.values().collect();
        chars.sort_by_key(|c| c.character_id);
        (
            chars[0].character_id,
            chars[1].character_id,
            chars[1].corporation_id,
        )
    });
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord {
            corporation_id: corp1,
            name: "P6 Corp".to_string(),
            ticker: "P6C".to_string(),
            ceo_id: char1,
            tax_rate: 0.11, // PvE bounty tax rate — must NOT be reported as facility industry tax
            master_wallet: IskCents::from_isk(500_000_000),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp1, corp);
        snap.skills.entry(char1).or_default().insert(
            3387,
            TrainedSkillRecord { type_id: 3387, level: 5, skill_points: 256_000.0 },
        );

        for (sid, tid, name) in [
            (raitaru_id, 35_825, "P6 Perimeter Raitaru"),
            (bare_astrahus_id, 35_832, "P6 Bare Astrahus"),
        ] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: tid,
                    owner_corp_id: corp1,
                    solar_system_id: 30_000_144,
                    name: name.to_string(),
                    position: [1_000.0, 2_000.0, 3_000.0],
                    state: "ShieldVulnerable".to_string(),
                    quantum_core_installed: true,
                    fuel_blocks: 500,
                    shield_hp: 10_000.0,
                    armor_hp: 10_000.0,
                    hull_hp: 10_000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
        }
        for (iid, tid, owner, loc, flag) in [
            (990_001, 35_878, corp1, raitaru_id, InventoryFlag::ServiceSlot0),
            (990_002, 691, char1, station_id, InventoryFlag::Hangar),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        save_facility_taxes(snap, raitaru_id, &HashMap::from([(1_u8, 0.035)]));
        Ok(())
    });

    let mut sess1 = setup_session(&ctx, char1, corp1, station_id, ROLE_FACTORY_MANAGER | ROLE_DIRECTOR);
    let _sess2 = setup_session(&ctx, char2, corp2, station_id, 0);

    // Seed sufficient materials for 2 Rifter (691) manufacturing runs in Jita hangar
    let rifter_mats: Vec<(u32, i32)> = ctx.sde.blueprints[&691]
        .materials
        .iter()
        .map(|m| (m.type_id, (m.quantity as i32) * 10))
        .collect();
    for (tid, qty) in rifter_mats {
        seed_hangar_item(&ctx, char1, station_id, tid, qty);
    }

    let job_args = [PyRep::Integer(691), PyRep::Integer(1), PyRep::Integer(1)];
    let job_res = ctx
        .dispatch_rpc(&mut sess1, "industryManager", None, "InstallJob", &job_args, &empty, None)
        .await
        .expect("InstallJob");
    let PyRep::KeyVal(job_kv) = job_res else { panic!("expected KeyVal") };
    let job_id = job_kv.get("jobID").unwrap().as_i64().unwrap();

    let corp_kwargs = HashMap::from([("useCorp".to_string(), PyRep::Bool(true))]);
    let corp_job_res = ctx
        .dispatch_rpc(&mut sess1, "industryManager", None, "InstallJob", &job_args, &corp_kwargs, None)
        .await
        .expect("InstallJob useCorp");
    let PyRep::KeyVal(corp_job_kv) = corp_job_res else { panic!("expected KeyVal") };
    let corp_job_id = corp_job_kv.get("jobID").unwrap().as_i64().unwrap();

    // Public /industry/facilities endpoint: exact tax assertions + non-industry structure exclusion
    let facilities = get_json(&router, &spec, "/v1/industry/facilities/", None);
    let fac_arr = facilities.as_array().unwrap();
    let jita_fac = fac_arr.iter().find(|f| f["facility_id"] == station_id).expect("Jita facility");
    assert_eq!(jita_fac["tax"], 0.0025);
    let raitaru_fac = fac_arr.iter().find(|f| f["facility_id"] == raitaru_id).expect("Raitaru facility");
    assert_eq!(raitaru_fac["tax"], 0.035);
    assert!(!fac_arr.iter().any(|f| f["facility_id"] == bare_astrahus_id));

    let tok_char = issue_token_with_scopes(&ctx, char1, vec!["esi-industry.read_character_jobs.v1"]);
    let tok_corp = issue_token_with_scopes(&ctx, char1, vec!["esi-industry.read_corporation_jobs.v1"]);
    let tok_other = issue_token_with_scopes(&ctx, char2, vec!["esi-industry.read_character_jobs.v1"]);

    assert_status(&router, format!("/v1/characters/{char1}/industry/jobs/"), None, 401);
    assert_status(&router, format!("/v1/characters/{char1}/industry/jobs/"), Some(&tok_other), 403);
    assert_status(&router, format!("/v1/corporations/{corp1}/industry/jobs/"), None, 401);
    assert_status(&router, format!("/v1/corporations/{corp1}/industry/jobs/"), Some(&tok_other), 403);

    // Personal vs corporation job isolation while both jobs are active
    let char_jobs = get_json(&router, &spec, format!("/v1/characters/{char1}/industry/jobs/"), Some(&tok_char));
    let char_jobs_arr = char_jobs.as_array().unwrap();
    let j = char_jobs_arr.iter().find(|j| j["job_id"] == job_id).expect("personal job in character jobs");
    assert_eq!(j["status"], "active");
    assert_eq!(j["facility_id"], station_id);
    assert!(!char_jobs_arr.iter().any(|j| j["job_id"] == corp_job_id));

    let corp_jobs = get_json(&router, &spec, format!("/v1/corporations/{corp1}/industry/jobs/"), Some(&tok_corp));
    let corp_jobs_arr = corp_jobs.as_array().unwrap();
    assert!(corp_jobs_arr.iter().any(|j| j["job_id"] == corp_job_id));
    assert!(!corp_jobs_arr.iter().any(|j| j["job_id"] == job_id));

    // Complete personal job and verify include_completed=false vs true + continued corp isolation
    ctx.advance_virtual_clock(6_000);
    let tok_char = issue_token_with_scopes(&ctx, char1, vec!["esi-industry.read_character_jobs.v1"]);
    let tok_corp = issue_token_with_scopes(&ctx, char1, vec!["esi-industry.read_corporation_jobs.v1"]);
    ctx.dispatch_rpc(&mut sess1, "industryManager", None, "CompleteJob", &[PyRep::Integer(job_id)], &empty, None)
        .await
        .expect("CompleteJob");

    let active_only = get_json(&router, &spec, format!("/v1/characters/{char1}/industry/jobs/?include_completed=false"), Some(&tok_char));
    assert!(!active_only.as_array().unwrap().iter().any(|j| j["job_id"] == job_id));

    let all_jobs = get_json(&router, &spec, format!("/v1/characters/{char1}/industry/jobs/?include_completed=true"), Some(&tok_char));
    let all_char_arr = all_jobs.as_array().unwrap();
    let completed = all_char_arr.iter().find(|j| j["job_id"] == job_id).expect("completed job");
    assert_eq!(completed["status"], "delivered");
    assert_eq!(completed["completed_character_id"], char1);
    assert!(!all_char_arr.iter().any(|j| j["job_id"] == corp_job_id));

    let corp_all = get_json(&router, &spec, format!("/v1/corporations/{corp1}/industry/jobs/?include_completed=true"), Some(&tok_corp));
    let corp_all_arr = corp_all.as_array().unwrap();
    assert!(corp_all_arr.iter().any(|j| j["job_id"] == corp_job_id));
    assert!(!corp_all_arr.iter().any(|j| j["job_id"] == job_id));

    // SQLite persistence check
    drop(router);
    drop(ctx);
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo2));
    let router2 = EsiRouter::new(ctx2);
    let tok_char2 = issue_token_with_scopes(&router2.ctx, char1, vec!["esi-industry.read_character_jobs.v1"]);
    let persisted = get_json(&router2, &spec, format!("/v1/characters/{char1}/industry/jobs/?include_completed=true"), Some(&tok_char2));
    assert!(persisted.as_array().unwrap().iter().any(|j| j["job_id"] == job_id));
    assert!(!persisted.as_array().unwrap().iter().any(|j| j["job_id"] == corp_job_id));
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_esi_corp_roles_personnel_manager_and_grantable_role_gate() {
    let (db_path, ctx) = open_seeded_sqlite_ctx("corp_roles_gate");
    let router = EsiRouter::new(ctx.clone());
    let spec = OpenApiSpec::pinned();
    let (corp1, ceo_id, plain_id, trader_id, pm_id, grantable_id) =
        (98_000_077_i64, 90_000_001_i64, 90_000_801_i64, 90_000_802_i64, 90_000_803_i64, 90_000_804_i64);

    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        let base = snap.characters[&ceo_id].clone();
        let mut corp = CorporationRecord {
            corporation_id: corp1, name: "Roles Gate Corp".into(), ticker: "RGC".into(), ceo_id, ..Default::default()
        };
        for (cid, roles) in [
            (ceo_id, ROLE_DIRECTOR),
            (plain_id, 0),
            (trader_id, eve_db::corp_roles::ROLE_TRADER),
            (pm_id, eve_db::corp_roles::ROLE_PERSONNEL_MANAGER),
            (grantable_id, 0),
        ] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.corporation_id = corp1;
            snap.characters.insert(cid, ch);
            corp.members.insert(cid);
            corp.member_roles.insert(cid, roles);
        }
        snap.corporations.insert(corp1, corp);
        snap.corp_role_assignments.insert(
            (corp1, grantable_id),
            eve_db::CorpMemberRoleAssignment { character_id: grantable_id, grantable_roles: ROLE_FACTORY_MANAGER, ..Default::default() },
        );
        Ok(())
    });

    let tok_for = |cid: i64| issue_token_with_scopes(&ctx, cid, vec!["esi-corporations.read_corporation_membership.v1"]);
    let (tok_plain, tok_trader, tok_pm, tok_grantable, tok_ceo) =
        (tok_for(plain_id), tok_for(trader_id), tok_for(pm_id), tok_for(grantable_id), tok_for(ceo_id));

    assert_status(&router, format!("/v1/corporations/{corp1}/roles/"), Some(&tok_plain), 403);
    assert_status(&router, format!("/v1/corporations/{corp1}/roles/"), Some(&tok_trader), 403);
    assert_status(&router, format!("/v1/corporations/{corp1}/roles/history/"), Some(&tok_plain), 403);
    assert_status(&router, format!("/v1/corporations/{corp1}/roles/history/"), Some(&tok_pm), 403);

    for tok in [&tok_pm, &tok_grantable, &tok_ceo] {
        let arr = get_json(&router, &spec, format!("/v1/corporations/{corp1}/roles/"), Some(tok));
        assert!(arr.as_array().unwrap().iter().any(|r| r["character_id"] == grantable_id));
    }
    let _ = get_json(&router, &spec, format!("/v1/corporations/{corp1}/roles/history/"), Some(&tok_ceo));
    let _ = std::fs::remove_file(&db_path);
}

}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod esi_p6_ext_b {
    use super::*;

// `ESI-6` live HTTP + RPC + SQLite persistence regression tests for contract party isolation,
// public contract `204` lifecycle transitions, and 30-day contract list retention.


use crate::esi_market_industry_pi::{assert_status, get_json, issue_token_with_scopes};
use eve_db::{
    corp_roles::{ROLE_CONTRACT_MANAGER, ROLE_DIRECTOR},
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

fn temp_sqlite_path(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("evomu_p6_esi1_pt2_{tag}_{nanos}.sqlite"))
}

fn open_seeded_sqlite_ctx(tag: &str) -> (std::path::PathBuf, Arc<ServerContext>) {
    let db_path = temp_sqlite_path(tag);
    let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
    let ctx = Arc::new(ServerContext::with_repository(repo));
    let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    ctx.db
        .try_with_mut_snapshot(|snap| {
            *snap = demo;
            Ok::<_, std::convert::Infallible>(())
        })
        .unwrap();
    crate::common::pin_clock(&ctx);
    (db_path, ctx)
}

fn setup_session(
    ctx: &ServerContext,
    char_id: i64,
    corp_id: i64,
    station_id: i64,
    roles: u64,
) -> SessionState {
    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(station_id);
    sess.solar_system_id = Some(30_000_142);
    sess.region_id = Some(10_000_002);
    sess.corp_role = roles;
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_isk(500_000_000);
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(char_id);
            corp.member_roles.insert(char_id, roles);
        }
        Ok(())
    });
    sess
}

fn seed_hangar_item(
    ctx: &ServerContext,
    owner_id: i64,
    station_id: i64,
    type_id: u32,
    qty: i32,
) -> i64 {
    ctx.db
        .try_with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let iid = snap.next_entity_id;
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id,
                    owner_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            Ok::<_, std::convert::Infallible>(iid)
        })
        .unwrap()
}

#[allow(clippy::too_many_arguments)]
async fn create_contract_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    ctype: i64,
    num_days: i64,
    start_station: i64,
    end_station: i64,
    price: f64,
    reward: f64,
    collateral_or_buyout: f64,
    title: &str,
    item_id: i64,
    qty: i64,
    for_corp: bool,
) -> i64 {
    let mut kw = HashMap::new();
    if for_corp {
        kw.insert("forCorp".to_string(), PyRep::Bool(true));
    }
    let PyRep::KeyVal(kv) = ctx
        .dispatch_rpc(
            sess,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(ctype),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(num_days),
                PyRep::Integer(start_station),
                PyRep::Integer(end_station),
                PyRep::Real(price),
                PyRep::Real(reward),
                PyRep::Real(collateral_or_buyout),
                PyRep::String(title.into()),
                PyRep::String(String::new()),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(item_id),
                    PyRep::Integer(qty),
                ])]),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateContract")
    else {
        panic!("expected KeyVal from CreateContract");
    };
    kv.get("contractID").unwrap().as_i64().unwrap()
}

fn assert_204_valid(router: &EsiRouter, spec: &OpenApiSpec, target: impl Into<String>) {
    let req = EsiHttpRequest::new("GET", target);
    let resp = router.handle_request(&req);
    let raw_path = req.raw_target.split('?').next().unwrap_or(&req.raw_target);
    assert_eq!(
        resp.status_code,
        204,
        "Expected 204 for GET {}, got {} ({})",
        req.raw_target,
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
    assert!(resp.body.is_empty(), "204 response body must be empty");
    spec.validate_response("GET", raw_path, 204, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI 204 validation failed for {}: {e}", req.raw_target));
}

#[tokio::test]
async fn test_esi_p6_char_contract_party_isolation_and_bidder_privacy() {
    // parity: esi:GET /characters/{character_id}/contracts/{contract_id}/bids, esi:GET /characters/{character_id}/contracts/{contract_id}/items, esi:GET /contracts/public/bids/{contract_id}, esi:GET /contracts/public/items/{contract_id}, esi:GET /corporations/{corporation_id}/contracts/{contract_id}/bids, esi:GET /corporations/{corporation_id}/contracts/{contract_id}/items
    let (db_path, ctx) = open_seeded_sqlite_ctx("party_privacy");
    let router = EsiRouter::new(ctx.clone());
    let spec = OpenApiSpec::pinned();
    let empty = HashMap::new();

    let station_id = 60_003_760_i64;
    let corp1 = 98_000_501_i64;
    let char3 = 90_000_888_i64;
    let corp3 = 98_000_503_i64;
    let (char1, char2, corp2) = ctx.db.with_snapshot(|snap| {
        let mut chars: Vec<_> = snap.characters.values().collect();
        chars.sort_by_key(|c| c.character_id);
        (
            chars[0].character_id,
            chars[1].character_id,
            chars[1].corporation_id,
        )
    });
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        let mut c1 = CorporationRecord {
            corporation_id: corp1,
            name: "Party Isolation Corp".into(),
            ticker: "PIC1".into(),
            ceo_id: char1,
            master_wallet: IskCents::from_isk(500_000_000),
            ..Default::default()
        };
        c1.ensure_wallet_divisions();
        snap.corporations.insert(corp1, c1);

        let c3 = CorporationRecord {
            corporation_id: corp3,
            name: "Third Party Corp".into(),
            ticker: "TPC3".into(),
            ceo_id: char3,
            master_wallet: IskCents::from_isk(100_000_000),
            ..Default::default()
        };
        snap.corporations.insert(corp3, c3);
        let mut bystander = snap.characters[&char1].clone();
        bystander.character_id = char3;
        bystander.name = "Unrelated Bystander".into();
        bystander.corporation_id = corp3;
        bystander.station_id = Some(station_id);
        bystander.balance = IskCents::from_isk(100_000_000);
        snap.characters.insert(char3, bystander);
        Ok(())
    });

    let mut sess1 = setup_session(
        &ctx,
        char1,
        corp1,
        station_id,
        ROLE_CONTRACT_MANAGER | ROLE_DIRECTOR,
    );
    let mut sess2 = setup_session(&ctx, char2, corp2, station_id, 0);
    let _sess3 = setup_session(&ctx, char3, corp3, station_id, 0);

    let personal_item = seed_hangar_item(&ctx, char1, station_id, 34, 400);
    let corp_item = seed_hangar_item(&ctx, char1, station_id, 35, 300);

    // 1. Personal public auction issued by char1 (`forCorp == false`) with bid by char2
    let personal_cid = create_contract_rpc(
        &ctx,
        &mut sess1,
        2,
        1,
        station_id,
        station_id,
        100_000.0,
        0.0,
        500_000.0,
        "Personal Public Auction",
        personal_item,
        400,
        false,
    )
    .await;

    ctx.dispatch_rpc(
        &mut sess2,
        "contractProxy",
        None,
        "PlaceBid",
        &[PyRep::Integer(personal_cid), PyRep::Real(150_000.0)],
        &empty,
        None,
    )
    .await
    .expect("PlaceBid personal auction");

    // 2. Corporate public auction issued by char1 (`forCorp == true`) with bid by char2
    let corp_cid = create_contract_rpc(
        &ctx,
        &mut sess1,
        2,
        1,
        station_id,
        station_id,
        200_000.0,
        0.0,
        900_000.0,
        "Corp Public Auction",
        corp_item,
        300,
        true,
    )
    .await;

    ctx.dispatch_rpc(
        &mut sess2,
        "contractProxy",
        None,
        "PlaceBid",
        &[PyRep::Integer(corp_cid), PyRep::Real(275_000.0)],
        &empty,
        None,
    )
    .await
    .expect("PlaceBid corp auction");

    let tok1 = issue_token_with_scopes(
        &ctx,
        char1,
        vec![
            "esi-contracts.read_character_contracts.v1",
            "esi-contracts.read_corporation_contracts.v1",
        ],
    );
    let tok3 = issue_token_with_scopes(&ctx, char3, vec!["esi-contracts.read_character_contracts.v1"]);

    // Unrelated character `char3` gets 403 on `/characters/{char3}/contracts/{cid}/{items,bids}` for BOTH public contracts
    for cid in [personal_cid, corp_cid] {
        assert_status(
            &router,
            format!("/v1/characters/{char3}/contracts/{cid}/items/"),
            Some(&tok3),
            403,
        );
        assert_status(
            &router,
            format!("/v1/characters/{char3}/contracts/{cid}/bids/"),
            Some(&tok3),
            403,
        );
    }

    // `for_corp == true` issuer `char1` gets 403 on `/characters/{char1}/contracts/{corp_cid}/{items,bids}`
    assert_status(
        &router,
        format!("/v1/characters/{char1}/contracts/{corp_cid}/items/"),
        Some(&tok1),
        403,
    );
    assert_status(
        &router,
        format!("/v1/characters/{char1}/contracts/{corp_cid}/bids/"),
        Some(&tok1),
        403,
    );

    // Public bids endpoint strips `bidder_id`, while personal issuer & corp routes preserve `bidder_id`
    let pub_bids = get_json(&router, &spec, format!("/v1/contracts/public/bids/{personal_cid}/"), None);
    assert_eq!(pub_bids[0]["amount"], 150_000.0);
    assert!(pub_bids[0].get("bidder_id").is_none(), "public bids must omit bidder_id");

    let issuer_bids = get_json(
        &router,
        &spec,
        format!("/v1/characters/{char1}/contracts/{personal_cid}/bids/"),
        Some(&tok1),
    );
    assert_eq!(issuer_bids[0]["bidder_id"], char2);

    let corp_bids = get_json(
        &router,
        &spec,
        format!("/v1/corporations/{corp1}/contracts/{corp_cid}/bids/"),
        Some(&tok1),
    );
    assert_eq!(corp_bids[0]["bidder_id"], char2);

    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_esi_p6_public_contract_204_on_accept_and_expiry_and_30d_retention() {
    // parity: esi:GET /characters/{character_id}/contracts, esi:GET /contracts/public/{region_id}, esi:GET /contracts/public/bids/{contract_id}, esi:GET /contracts/public/items/{contract_id}, esi:GET /corporations/{corporation_id}/contracts
    let (db_path, ctx) = open_seeded_sqlite_ctx("lifecycle_retention");
    let router = EsiRouter::new(ctx.clone());
    let spec = OpenApiSpec::pinned();
    let empty = HashMap::new();

    let start_station = 60_003_760_i64;
    let dest_station = 60_003_757_i64;
    let region_id = 10_000_002_i64;
    let corp1 = 98_000_511_i64;
    let (char1, char2, corp2) = ctx.db.with_snapshot(|snap| {
        let mut chars: Vec<_> = snap.characters.values().collect();
        chars.sort_by_key(|c| c.character_id);
        (
            chars[0].character_id,
            chars[1].character_id,
            chars[1].corporation_id,
        )
    });
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        let mut c1 = CorporationRecord {
            corporation_id: corp1,
            name: "Retention Corp".into(),
            ticker: "RETC".into(),
            ceo_id: char1,
            master_wallet: IskCents::from_isk(500_000_000),
            ..Default::default()
        };
        c1.ensure_wallet_divisions();
        snap.corporations.insert(corp1, c1);
        Ok(())
    });

    let mut sess1 = setup_session(
        &ctx,
        char1,
        corp1,
        start_station,
        ROLE_CONTRACT_MANAGER | ROLE_DIRECTOR,
    );
    let mut sess2 = setup_session(&ctx, char2, corp2, start_station, 0);

    let exchange_item = seed_hangar_item(&ctx, char1, start_station, 34, 200);
    let expiring_item = seed_hangar_item(&ctx, char1, start_station, 35, 150);
    let courier_item = seed_hangar_item(&ctx, char1, start_station, 36, 50);
    let corp_exchange_item = seed_hangar_item(&ctx, char1, start_station, 37, 100);

    let accepted_cid = create_contract_rpc(
        &ctx,
        &mut sess1,
        1,
        1,
        start_station,
        start_station,
        50_000.0,
        0.0,
        0.0,
        "Accepted Exchange",
        exchange_item,
        200,
        false,
    )
    .await;
    let expiring_cid = create_contract_rpc(
        &ctx,
        &mut sess1,
        2,
        1,
        start_station,
        start_station,
        100_000.0,
        0.0,
        400_000.0,
        "Expiring Auction",
        expiring_item,
        150,
        false,
    )
    .await;
    let courier_cid = create_contract_rpc(
        &ctx,
        &mut sess1,
        3,
        28,
        start_station,
        dest_station,
        0.0,
        25_000.0,
        10_000.0,
        "Long InProgress Courier",
        courier_item,
        50,
        false,
    )
    .await;
    let corp_cid = create_contract_rpc(
        &ctx,
        &mut sess1,
        1,
        1,
        start_station,
        start_station,
        60_000.0,
        0.0,
        0.0,
        "Corp Exchange",
        corp_exchange_item,
        100,
        true,
    )
    .await;

    // While Outstanding & unexpired, public sub-routes return 200 OK
    assert_eq!(
        get_json(&router, &spec, format!("/v1/contracts/public/items/{accepted_cid}/"), None)
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        get_json(&router, &spec, format!("/v1/contracts/public/items/{expiring_cid}/"), None)
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Accept contracts A, C, and D via live `contractProxy.AcceptContract`
    for cid in [accepted_cid, courier_cid, corp_cid] {
        ctx.dispatch_rpc(
            &mut sess2,
            "contractProxy",
            None,
            "AcceptContract",
            &[PyRep::Integer(cid), PyRep::Bool(false)],
            &empty,
            None,
        )
        .await
        .expect("AcceptContract");
    }

    // Accepted contracts immediately return 204 No Content on `/contracts/public/{items,bids}/{cid}`
    assert_204_valid(&router, &spec, format!("/v1/contracts/public/items/{accepted_cid}/"));
    assert_204_valid(&router, &spec, format!("/v1/contracts/public/bids/{accepted_cid}/"));
    assert_204_valid(&router, &spec, format!("/v1/contracts/public/items/{courier_cid}/"));

    // Advance clock by 2 days -> `expiring_cid` expires; public region omits it and public items/bids return 204
    ctx.db.clock().advance_secs(2 * 86_400);
    let pub_after_2d = get_json(&router, &spec, format!("/v1/contracts/public/{region_id}/"), None);
    assert!(
        !pub_after_2d
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["contract_id"] == expiring_cid || c["contract_id"] == accepted_cid)
    );
    assert_204_valid(&router, &spec, format!("/v1/contracts/public/items/{expiring_cid}/"));
    assert_204_valid(&router, &spec, format!("/v1/contracts/public/bids/{expiring_cid}/"));

    // Within 30 days (`+2d`), finished/expired contracts are still retained in character & corporation lists
    let tok1 = issue_token_with_scopes(
        &ctx,
        char1,
        vec![
            "esi-contracts.read_character_contracts.v1",
            "esi-contracts.read_corporation_contracts.v1",
        ],
    );
    let char_2d = get_json(&router, &spec, format!("/v1/characters/{char1}/contracts/"), Some(&tok1));
    let char_2d_ids: Vec<i64> = char_2d
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["contract_id"].as_i64())
        .collect();
    assert!(char_2d_ids.contains(&accepted_cid));
    assert!(char_2d_ids.contains(&expiring_cid));
    assert!(char_2d_ids.contains(&courier_cid));

    let corp_2d = get_json(&router, &spec, format!("/v1/corporations/{corp1}/contracts/"), Some(&tok1));
    assert!(corp_2d.as_array().unwrap().iter().any(|c| c["contract_id"] == corp_cid));

    // Advance clock past 30 days (`+29d` -> total `+31d`): non-`in_progress` contracts older than 30 days are pruned,
    // while `in_progress` courier contract C remains visible.
    ctx.db.clock().advance_secs(29 * 86_400);
    let tok1_31d = issue_token_with_scopes(
        &ctx,
        char1,
        vec![
            "esi-contracts.read_character_contracts.v1",
            "esi-contracts.read_corporation_contracts.v1",
        ],
    );
    let char_31d = get_json(
        &router,
        &spec,
        format!("/v1/characters/{char1}/contracts/"),
        Some(&tok1_31d),
    );
    let char_31d_ids: Vec<i64> = char_31d
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["contract_id"].as_i64())
        .collect();
    assert!(
        !char_31d_ids.contains(&accepted_cid) && !char_31d_ids.contains(&expiring_cid),
        "finished/expired contracts > 30 days old must be excluded from character contracts"
    );
    assert!(
        char_31d_ids.contains(&courier_cid),
        "in_progress courier contract must remain visible past 30 days"
    );

    let corp_31d = get_json(
        &router,
        &spec,
        format!("/v1/corporations/{corp1}/contracts/"),
        Some(&tok1_31d),
    );
    assert!(
        !corp_31d.as_array().unwrap().iter().any(|c| c["contract_id"] == corp_cid),
        "completed corporation contract > 30 days old must be excluded from corporation contracts"
    );

    // Verify SQLite persistence across reopen
    drop(router);
    drop(ctx);
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo2));
    crate::common::pin_clock(&ctx2);
    ctx2.db.clock().advance_secs(31 * 86_400);
    let router2 = EsiRouter::new(ctx2);
    assert_204_valid(&router2, &spec, format!("/v1/contracts/public/items/{accepted_cid}/"));
    let tok1_reopen = issue_token_with_scopes(
        &router2.ctx,
        char1,
        vec!["esi-contracts.read_character_contracts.v1"],
    );
    let char_reopen = get_json(
        &router2,
        &spec,
        format!("/v1/characters/{char1}/contracts/"),
        Some(&tok1_reopen),
    );
    let reopen_ids: Vec<i64> = char_reopen
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["contract_id"].as_i64())
        .collect();
    assert!(reopen_ids.contains(&courier_cid));
    assert!(!reopen_ids.contains(&accepted_cid));
    let _ = std::fs::remove_file(&db_path);
}

}
