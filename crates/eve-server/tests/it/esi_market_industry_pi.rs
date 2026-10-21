//! `ESI-6` integration tests for Market, Contracts, Industry, Mining, and Planetary Interaction
//! routes against live `WorldRepository` and `SdeRegistry` state, validated against `OpenApiSpec::pinned()`.

#![forbid(unsafe_code)]

use eve_db::{
    AllianceRecord, ContractRecord, CorporationRecord, IndustryJobRecord, InventoryFlag,
    InventoryItemRecord, IskCents, MarketOrderRecord, MiningLedgerRecord, MoonExtractionRecord,
    ObserverMiningRecord, PlanetaryColonyRecord, UpwellStructureRecord,
};
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token_with_scopes(ctx: &ServerContext, character_id: i64, scopes: Vec<&str>) -> String {
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&character_id) {
            ch.user_id = 1;
        }
        Ok(())
    });
    let now = ctx.now_unix();
    let scope_vec: Vec<String> = scopes.into_iter().map(str::to_string).collect();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            character_id,
            "evomu-esi6-test",
            "https://localhost/callback",
            scope_vec,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    let bundle = ctx
        .db
        .exchange_pkce_code(&code, "evomu-esi6-test", PKCE_VERIFIER, now)
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
    let raw_path = req.raw_target.split('?').next().unwrap_or(&req.raw_target);
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
    assert!(
        resp.header("ETag").is_some(),
        "200 response for {} must include ETag header",
        req.raw_target
    );
    assert!(
        resp.header("Expires").is_some() || resp.header("Cache-Control").is_some(),
        "200 response for {} must include cache headers",
        req.raw_target
    );
    resp.json().expect("valid JSON response")
}

#[tokio::test]
async fn test_esi6_market_public_and_structure_routes() {
    // parity: esi:GET /markets/groups, esi:GET /markets/groups/{market_group_id}
    // parity: esi:GET /markets/prices, esi:GET /markets/{region_id}/types
    // parity: feat:esi.market.groups, feat:esi.market.prices, feat:esi.market.types
    eve_testkit::attest(&[
        "esi:GET /markets/groups",
        "esi:GET /markets/groups/{market_group_id}",
        "esi:GET /markets/prices",
        "esi:GET /markets/{region_id}/types",
        "feat:esi.market.groups",
        "feat:esi.market.prices",
        "feat:esi.market.types",
    ]);

    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let char_id = 90_000_001_i64;
    let rival_char_id = 90_000_002_i64;
    let corp_id = 98_000_601_i64;
    let rival_corp_id = 98_000_602_i64;
    let alliance_id = 99_000_601_i64;
    let structure_id = 1_030_000_601_001_i64;
    let private_structure_id = 1_030_000_601_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
            ch.standings.insert(rival_corp_id, -5.0);
        }
        if let Some(ch2) = snap.characters.get_mut(&rival_char_id) {
            ch2.corporation_id = rival_corp_id;
            ch2.alliance_id = None;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "ESI6 Market Corp".to_string(),
                ticker: "E6MK".to_string(),
                ceo_id: char_id,
                alliance_id: Some(alliance_id),
                members: HashSet::from([char_id]),
                ..Default::default()
            },
        );
        snap.corporations.insert(
            rival_corp_id,
            CorporationRecord {
                corporation_id: rival_corp_id,
                name: "Rival Market Corp".to_string(),
                ticker: "RVMK".to_string(),
                ceo_id: rival_char_id,
                alliance_id: None,
                members: HashSet::from([rival_char_id]),
                ..Default::default()
            },
        );
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "ESI6 Market Alliance".to_string(),
                short_name: "E6MA".to_string(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Jita Trade Citadel".to_string(),
                position: [1000.0, 2000.0, 3000.0],
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        snap.structures.insert(
            private_structure_id,
            UpwellStructureRecord {
                structure_id: private_structure_id,
                type_id: 35832,
                owner_corp_id: rival_corp_id,
                solar_system_id: 30_000_142,
                name: "Private Rival Citadel".to_string(),
                position: [5000.0, 6000.0, 7000.0],
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        snap.market_orders.clear();
        snap.market_orders.insert(
            6101,
            MarketOrderRecord {
                order_id: 6101,
                type_id: 34, // Tritanium
                owner_id: char_id,
                station_id: 60_003_760,
                region_id: 10_000_002,
                price: IskCents::from_isk(6),
                vol_entered: 10_000,
                vol_remaining: 7_500,
                min_volume: 1,
                is_bid: false,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            6102,
            MarketOrderRecord {
                order_id: 6102,
                type_id: 587, // Rifter
                owner_id: char_id,
                station_id: structure_id,
                region_id: 10_000_002,
                price: IskCents::from_isk(420_000),
                vol_entered: 5,
                vol_remaining: 3,
                min_volume: 1,
                is_bid: true,
                is_npc: false,
                escrow: IskCents::from_isk(1_260_000),
            },
        );
    });

    // 1. GET /v1/markets/groups/ and /v1/markets/groups/{market_group_id}/
    let groups_req = EsiHttpRequest::new("GET", "/v1/markets/groups/");
    let groups_resp = router.handle_request(&groups_req);
    assert_eq!(groups_resp.status_code, 200);
    let groups_etag = groups_resp.header("ETag").expect("ETag").to_string();
    let groups_json = validate_and_json(&router, &spec, &groups_req, 200);
    let groups_arr = groups_json.as_array().expect("groups array");
    assert!(groups_arr.len() > 100);
    let first_mg_id = groups_arr[0].as_i64().expect("market_group_id");

    let cond_groups = EsiHttpRequest::new("GET", "/v1/markets/groups/")
        .with_header("If-None-Match", &groups_etag);
    assert_eq!(router.handle_request(&cond_groups).status_code, 304);

    let mg_detail = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/markets/groups/{first_mg_id}/")),
        200,
    );
    assert_eq!(mg_detail["market_group_id"], json!(first_mg_id));
    let expected_sde_mg = ctx
        .sde
        .market_groups
        .get(&(first_mg_id as u32))
        .expect("SDE market group");
    assert_eq!(mg_detail["name"], json!(expected_sde_mg.name));
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("GET", "/v1/markets/groups/999999999/"))
            .status_code,
        404
    );

    // 2. GET /v1/markets/prices/
    let prices = validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v1/markets/prices/"), 200);
    let prices_arr = prices.as_array().unwrap();
    let trit_price = prices_arr
        .iter()
        .find(|p| p["type_id"] == json!(34))
        .expect("Tritanium price entry");
    assert_eq!(trit_price["average_price"], json!(6.0));
    let rifter_price = prices_arr
        .iter()
        .find(|p| p["type_id"] == json!(587))
        .expect("Rifter price entry");
    assert_eq!(rifter_price["average_price"], json!(420_000.0));

    // 3. GET /v1/markets/{region_id}/types/
    let types = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/markets/10000002/types/"),
        200,
    );
    let types_arr = types.as_array().unwrap();
    assert!(types_arr.contains(&json!(34)));
    assert!(types_arr.contains(&json!(587)));
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("GET", "/v1/markets/99999999/types/"))
            .status_code,
        404
    );
}

#[tokio::test]
async fn test_esi6_contracts_character_corporation_and_public_routes() {
    // parity: esi:GET /characters/{character_id}/contracts, esi:GET /characters/{character_id}/contracts/{contract_id}/items
    // parity: esi:GET /characters/{character_id}/contracts/{contract_id}/bids, esi:GET /contracts/public/{region_id}
    // parity: esi:GET /contracts/public/items/{contract_id}, esi:GET /contracts/public/bids/{contract_id}
    // parity: esi:GET /corporations/{corporation_id}/contracts, esi:GET /corporations/{corporation_id}/contracts/{contract_id}/items
    // parity: esi:GET /corporations/{corporation_id}/contracts/{contract_id}/bids
    // parity: feat:esi.contracts.char-list, feat:esi.contracts.char-items-bids, feat:esi.contracts.corp, feat:esi.contracts.public
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/contracts",
        "esi:GET /characters/{character_id}/contracts/{contract_id}/items",
        "esi:GET /characters/{character_id}/contracts/{contract_id}/bids",
        "esi:GET /contracts/public/{region_id}",
        "esi:GET /contracts/public/items/{contract_id}",
        "esi:GET /contracts/public/bids/{contract_id}",
        "esi:GET /corporations/{corporation_id}/contracts",
        "esi:GET /corporations/{corporation_id}/contracts/{contract_id}/items",
        "esi:GET /corporations/{corporation_id}/contracts/{contract_id}/bids",
        "feat:esi.contracts.char-list",
        "feat:esi.contracts.char-items-bids",
        "feat:esi.contracts.corp",
        "feat:esi.contracts.public",
    ]);

    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let now = ctx.now_unix();
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_621_i64;
    let public_contract_id = 6_301_i64;
    let private_contract_id = 6_302_i64;
    let escrow_item_id = 880_621_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "ESI6 Contracts Corp".to_string(),
                ticker: "E6CC".to_string(),
                ceo_id: char_id,
                members: HashSet::from([char_id]),
                ..Default::default()
            },
        );
        snap.items.insert(
            escrow_item_id,
            InventoryItemRecord {
                item_id: escrow_item_id,
                type_id: 34, // Tritanium
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 25_000,
                singleton: false,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            public_contract_id,
            ContractRecord {
                contract_id: public_contract_id,
                issuer_id: char_id,
                issuer_corp_id: corp_id,
                assignee_id: 0,
                acceptor_id: Some(90_000_002),
                contract_type: "Auction".to_string(),
                availability: "Public".to_string(),
                status: "Outstanding".to_string(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                price: IskCents::from_isk(1_000_000),
                reward: IskCents(0),
                collateral: IskCents(0),
                item_ids: vec![escrow_item_id],
                requested_items: vec![(35, 500)], // Pyerite requested
                expiration_unix: now + 86_400,
                current_bid: IskCents::from_isk(2_500_000),
                current_bidder_id: Some(90_000_002),
                bid_history: vec![
                    (90_000_002, IskCents::from_isk(1_500_000), now.saturating_sub(600)),
                    (90_000_001, IskCents::from_isk(2_500_000), now.saturating_sub(120)),
                ],
                ..Default::default()
            },
        );
        snap.contracts.insert(
            private_contract_id,
            ContractRecord {
                contract_id: private_contract_id,
                issuer_id: 90_000_002,
                issuer_corp_id: 1_000_009,
                assignee_id: 90_000_002,
                acceptor_id: None,
                contract_type: "ItemExchange".to_string(),
                availability: "Personal".to_string(),
                status: "Outstanding".to_string(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                price: IskCents::from_isk(500_000),
                expiration_unix: now + 86_400,
                ..Default::default()
            },
        );
    });

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-contracts.read_character_contracts.v1",
            "esi-contracts.read_corporation_contracts.v1",
        ],
    );

    // 1. Character contracts list, items (escrow + requested), and bids
    let char_contracts = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/contracts/"))
            .with_bearer_token(&token),
        200,
    );
    assert!(char_contracts
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["contract_id"] == json!(public_contract_id) && c["type"] == json!("auction")));

    let char_items = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/characters/{char_id}/contracts/{public_contract_id}/items/"),
        )
        .with_bearer_token(&token),
        200,
    );
    let char_items_arr = char_items.as_array().unwrap();
    assert_eq!(char_items_arr.len(), 2);
    assert!(char_items_arr
        .iter()
        .any(|i| i["type_id"] == json!(34) && i["quantity"] == json!(25_000) && i["is_included"] == json!(true)));
    assert!(char_items_arr
        .iter()
        .any(|i| i["type_id"] == json!(35) && i["quantity"] == json!(500) && i["is_included"] == json!(false)));

    // Non-party character must get 403 on private contract items
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new(
                    "GET",
                    format!("/v1/characters/{char_id}/contracts/{private_contract_id}/items/")
                )
                .with_bearer_token(&token)
            )
            .status_code,
        403
    );

    let char_bids = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/characters/{char_id}/contracts/{public_contract_id}/bids/"),
        )
        .with_bearer_token(&token),
        200,
    );
    assert_eq!(char_bids.as_array().unwrap().len(), 2);
    assert_eq!(char_bids[0]["amount"], json!(2_500_000.0));

    // 2. Public contracts list, items, and bids
    let pub_contracts = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/contracts/public/10000002/"),
        200,
    );
    assert!(pub_contracts
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["contract_id"] == json!(public_contract_id)));

    let pub_items = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/contracts/public/items/{public_contract_id}/"),
        ),
        200,
    );
    assert_eq!(pub_items.as_array().unwrap().len(), 2);
    // Private contract must return 404 on public items endpoint
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new(
                "GET",
                format!("/v1/contracts/public/items/{private_contract_id}/")
            ))
            .status_code,
        404
    );

    let pub_bids = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/contracts/public/bids/{public_contract_id}/"),
        ),
        200,
    );
    assert_eq!(pub_bids.as_array().unwrap().len(), 2);
    assert_eq!(pub_bids[0]["amount"], json!(2_500_000.0));

    // 3. Corporation contracts list, items, and bids
    let corp_contracts = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/contracts/"))
            .with_bearer_token(&token),
        200,
    );
    assert!(corp_contracts
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["contract_id"] == json!(public_contract_id)));

    let corp_items = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/corporations/{corp_id}/contracts/{public_contract_id}/items/"),
        )
        .with_bearer_token(&token),
        200,
    );
    assert_eq!(corp_items.as_array().unwrap().len(), 2);

    let corp_bids = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/corporations/{corp_id}/contracts/{public_contract_id}/bids/"),
        )
        .with_bearer_token(&token),
        200,
    );
    assert_eq!(corp_bids.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_esi6_industry_and_mining_routes() {
    // parity: esi:GET /characters/{character_id}/industry/jobs, esi:GET /characters/{character_id}/mining
    // parity: esi:GET /corporations/{corporation_id}/industry/jobs, esi:GET /corporation/{corporation_id}/mining/extractions
    // parity: esi:GET /corporation/{corporation_id}/mining/observers, esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}
    // parity: feat:esi.industry.char-jobs, feat:esi.industry.char-mining
    // parity: feat:esi.industry.corp-jobs, feat:esi.industry.corp-extractions, feat:esi.industry.corp-observers
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/industry/jobs",
        "esi:GET /characters/{character_id}/mining",
        "esi:GET /corporations/{corporation_id}/industry/jobs",
        "esi:GET /corporation/{corporation_id}/mining/extractions",
        "esi:GET /corporation/{corporation_id}/mining/observers",
        "esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}",
        "feat:esi.industry.char-jobs",
        "feat:esi.industry.char-mining",
        "feat:esi.industry.corp-jobs",
        "feat:esi.industry.corp-extractions",
        "feat:esi.industry.corp-observers",
    ]);

    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let now = ctx.now_unix();
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_631_i64;
    let struct_id = 1_030_000_631_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "ESI6 Industrial Corp".to_string(),
                ticker: "E6IC".to_string(),
                ceo_id: char_id,
                members: HashSet::from([char_id]),
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35835, // Athanor
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Jita Moon Refinery".to_string(),
                position: [1500.0, 2500.0, 3500.0],
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 400,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        snap.industry_jobs.insert(
            6401,
            IndustryJobRecord {
                job_id: 6401,
                installer_id: char_id,
                facility_id: 60_003_760,
                blueprint_item_id: 100_000_007,
                blueprint_type_id: 691, // Rifter Blueprint
                product_type_id: 587,   // Rifter
                activity_id: 1,
                runs: 3,
                duration_seconds: 7_200,
                start_unix: now.saturating_sub(300),
                status: "Active".to_string(),
            },
        );
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: char_id,
            solar_system_id: 30_000_142,
            type_id: 1228, // Scordite (within 30-day window)
            quantity: 6_400,
            date: "2025-12-16".to_string(),
            timestamp_unix: now,
        });
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: char_id,
            solar_system_id: 30_000_142,
            type_id: 1230, // Veldspar (45 days old — outside 30-day window)
            quantity: 1_500,
            date: "2025-11-01".to_string(),
            timestamp_unix: now.saturating_sub(45 * 86_400),
        });
        snap.moon_extractions.insert(
            struct_id,
            MoonExtractionRecord {
                structure_id: struct_id,
                moon_id: 40_009_082,
                extraction_start_unix: now.saturating_sub(3600),
                chunk_arrival_unix: now + 72_000,
                natural_decay_unix: now + 82_800,
                status: "Started".to_string(),
                auto_fracture: false,
                event_id: 1,
                moon_ores: vec![(45492, 12_000.0)],
                spawned_asteroid_ids: vec![],
            },
        );
        snap.observer_mining_ledger.insert(
            struct_id,
            vec![ObserverMiningRecord {
                observer_id: struct_id,
                character_id: char_id,
                recorded_corporation_id: corp_id,
                type_id: 45492,
                quantity: 9_250,
                last_updated_date: "2025-12-16".to_string(),
                timestamp_unix: now,
            }],
        );
    });

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-industry.read_character_jobs.v1",
            "esi-industry.read_character_mining.v1",
            "esi-industry.read_corporation_jobs.v1",
            "esi-industry.read_corporation_mining.v1",
        ],
    );

    let char_jobs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/industry/jobs/"))
            .with_bearer_token(&token),
        200,
    );
    assert_eq!(char_jobs[0]["job_id"], json!(6401));
    assert_eq!(char_jobs[0]["blueprint_type_id"], json!(691));
    assert_eq!(char_jobs[0]["product_type_id"], json!(587));
    assert_eq!(char_jobs[0]["runs"], json!(3));

    let char_mining = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/mining/"))
            .with_bearer_token(&token),
        200,
    );
    let char_mining_arr = char_mining.as_array().unwrap();
    assert!(char_mining_arr
        .iter()
        .any(|m| m["type_id"] == json!(1228) && m["quantity"] == json!(6_400)));
    assert!(
        !char_mining_arr.iter().any(|m| m["type_id"] == json!(1230)),
        "Mining ledger entries older than 30 days must be excluded"
    );

    let corp_jobs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/industry/jobs/"))
            .with_bearer_token(&token),
        200,
    );
    assert_eq!(corp_jobs[0]["job_id"], json!(6401));

    let extractions = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/corporation/{corp_id}/mining/extractions/"),
        )
        .with_bearer_token(&token),
        200,
    );
    assert_eq!(extractions[0]["structure_id"], json!(struct_id));
    assert_eq!(extractions[0]["moon_id"], json!(40_009_082));

    let observers = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/corporation/{corp_id}/mining/observers/"),
        )
        .with_bearer_token(&token),
        200,
    );
    assert!(observers
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["observer_id"] == json!(struct_id) && o["last_updated"] == json!("2025-12-16")));

    let obs_detail = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/corporation/{corp_id}/mining/observers/{struct_id}/"),
        )
        .with_bearer_token(&token),
        200,
    );
    assert_eq!(obs_detail[0]["character_id"], json!(char_id));
    assert_eq!(obs_detail[0]["quantity"], json!(9_250));
}

#[tokio::test]
async fn test_esi6_planetary_interaction_and_plan_closure() {
    // parity: esi:GET /characters/{character_id}/planets, esi:GET /universe/schematics/{schematic_id}
    // parity: feat:esi.planetary-interaction.colonies, feat:esi.planetary-interaction.schematics
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/planets",
        "esi:GET /universe/schematics/{schematic_id}",
        "feat:esi.planetary-interaction.colonies",
        "feat:esi.planetary-interaction.schematics",
    ]);

    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let now = ctx.now_unix();
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_641_i64;
    let planet_id = 40_009_080_i64; // Jita III (Planet (Temperate))

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "ESI6 Planetary Corp".to_string(),
                ticker: "E6PI".to_string(),
                ceo_id: char_id,
                members: HashSet::from([char_id]),
                ..Default::default()
            },
        );
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                owner_id: char_id,
                planet_id,
                solar_system_id: 30_000_142,
                command_center_level: 4,
                pins_count: 5,
                extracted_commodities: HashMap::from([(2393, 600)]),
                last_extracted_unix: now,
            },
        );
    });

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec!["esi-planets.manage_planets.v1"],
    );

    let planets = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/planets/"))
            .with_bearer_token(&token),
        200,
    );
    let planets_arr = planets.as_array().unwrap();
    assert_eq!(planets_arr.len(), 1);
    assert_eq!(planets_arr[0]["planet_id"], json!(planet_id));
    assert_eq!(planets_arr[0]["upgrade_level"], json!(4));
    assert_eq!(planets_arr[0]["num_pins"], json!(5));
    assert_eq!(planets_arr[0]["planet_type"], json!("temperate"));

    // Schematic lookup from real SDE planet_schematics
    let (&schem_id, schem_rec) = ctx
        .sde
        .planet_schematics
        .iter()
        .next()
        .expect("SDE must contain planet schematics");
    let schem_json = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/universe/schematics/{schem_id}/")),
        200,
    );
    assert_eq!(schem_json["schematic_name"], json!(schem_rec.name));
    assert_eq!(schem_json["cycle_time"], json!(schem_rec.cycle_time as i64));
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new(
                "GET",
                "/v1/universe/schematics/999999999/"
            ))
            .status_code,
        404
    );
}
