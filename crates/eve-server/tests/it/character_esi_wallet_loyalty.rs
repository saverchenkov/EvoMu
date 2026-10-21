//! Integration tests for `ESI-4`: Character Research Agents, Medals, Scoped Roles,
//! Loyalty Store Offers, and Character & Corporation Wallet Journal and Transactions.

use eve_db::{
    CharacterMedalGraphicRecord, CharacterMedalRecord, CharacterResearchAgentRecord,
    CharacterScopedRolesRecord, CorpWalletDivisionRecord, CorpWalletJournalRecord,
    CorporationRecord, IskCents, LoyaltyStoreOfferItemRecord, LoyaltyStoreOfferRecord,
    WalletJournalEntry, WalletTransactionRecord,
};
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token_with_scopes(ctx: &ServerContext, character_id: i64, scopes: Vec<String>) -> String {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&character_id) {
            ch.user_id = 1;
        }
    });
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            character_id,
            "evomu-esi-wallet-loyalty-test",
            "https://localhost/callback",
            scopes,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    let bundle = ctx
        .db
        .exchange_pkce_code(&code, "evomu-esi-wallet-loyalty-test", PKCE_VERIFIER, now)
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
    if expected_status == 204 || expected_status == 304 {
        Value::Null
    } else {
        resp.json().expect("valid JSON response")
    }
}

fn assert_etag_and_304(router: &EsiRouter, spec: &OpenApiSpec, req: &EsiHttpRequest) {
    let resp = router.handle_request(req);
    assert_eq!(resp.status_code, 200);
    let etag = resp
        .header("etag")
        .expect("ETag header must be present on 200 GET response")
        .to_string();
    assert!(
        resp.header("cache-control").is_some(),
        "Cache-Control header must be present"
    );
    let mut cond_req = req.clone();
    cond_req.headers.insert("if-none-match".to_string(), etag);
    validate_and_json(router, spec, &cond_req, 304);
}

#[tokio::test]
async fn test_esi4_character_agents_research_medals_roles_and_loyalty_offers() {
    // parity: esi:GET /characters/{character_id}/agents_research, feat:esi.character.agents-research
    // parity: esi:GET /characters/{character_id}/medals, feat:esi.character.medals
    // parity: esi:GET /characters/{character_id}/roles, feat:esi.character.roles
    // parity: esi:GET /loyalty/stores/{corporation_id}/offers, feat:esi.loyalty.offers
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/agents_research",
        "feat:esi.character.agents-research",
        "esi:GET /characters/{character_id}/medals",
        "feat:esi.character.medals",
        "esi:GET /characters/{character_id}/roles",
        "feat:esi.character.roles",
        "esi:GET /loyalty/stores/{corporation_id}/offers",
        "feat:esi.loyalty.offers",
    ]);

    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let char_id = 90_000_001;
    let other_char_id = 90_000_002;
    let corp_id = 98_000_401;
    let npc_corp_id = 1_000_035; // Caldari Navy

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Apex Research Collective".to_string(),
                ticker: "APXR".to_string(),
                ceo_id: char_id,
                members: HashSet::from([char_id]),
                member_roles: HashMap::from([(char_id, eve_db::corp_roles::ROLE_DIRECTOR)]),
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
    });

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-characters.read_agents_research.v1".to_string(),
            "esi-characters.read_medals.v1".to_string(),
            "esi-characters.read_corporation_roles.v1".to_string(),
        ],
    );
    let wrong_token = issue_token_with_scopes(&ctx, other_char_id, vec![]);

    // 1. GET /v2/characters/{character_id}/agents_research/
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/agents_research/")),
        401,
    );
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/agents_research/"))
            .with_bearer_token(&wrong_token),
        403,
    );
    let research_req =
        EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/agents_research/"))
            .with_bearer_token(&token);
    assert_eq!(validate_and_json(&router, &spec, &research_req, 200), json!([]));

    ctx.db.with_mut_snapshot(|snap| {
        snap.char_research_agents.insert(
            char_id,
            vec![
                CharacterResearchAgentRecord {
                    agent_id: 3_019_002,
                    skill_type_id: 11_446, // Graviton Physics
                    started_at_unix: 1_700_100_000,
                    points_per_day: 84.25,
                    remainder_points: 1_240.5,
                },
                CharacterResearchAgentRecord {
                    agent_id: 3_019_001,
                    skill_type_id: 11_442, // Aerospace Engineering
                    started_at_unix: 1_700_000_000,
                    points_per_day: 62.5,
                    remainder_points: 450.0,
                },
            ],
        );
    });
    assert_etag_and_304(&router, &spec, &research_req);
    let research_list = validate_and_json(&router, &spec, &research_req, 200);
    assert_eq!(
        research_list,
        json!([
            {
                "agent_id": 3_019_001,
                "skill_type_id": 11_442,
                "started_at": "2023-11-14T22:13:20Z",
                "points_per_day": 62.5,
                "remainder_points": 450.0
            },
            {
                "agent_id": 3_019_002,
                "skill_type_id": 11_446,
                "started_at": "2023-11-16T02:00:00Z",
                "points_per_day": 84.25,
                "remainder_points": 1_240.5
            }
        ])
    );

    // 2. GET /v2/characters/{character_id}/medals/
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/medals/")),
        401,
    );
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/medals/"))
            .with_bearer_token(&wrong_token),
        403,
    );
    let medals_req = EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/medals/"))
        .with_bearer_token(&token);
    assert_eq!(validate_and_json(&router, &spec, &medals_req, 200), json!([]));

    ctx.db.with_mut_snapshot(|snap| {
        snap.char_medals.insert(
            char_id,
            vec![CharacterMedalRecord {
                medal_id: 501,
                title: "Star of New Eden".to_string(),
                description: "Awarded for fleet vanguard valor".to_string(),
                corporation_id: corp_id,
                issuer_id: other_char_id,
                date_unix: 1_700_000_000,
                reason: "Defended staging citadel".to_string(),
                status: "public".to_string(),
                graphics: vec![CharacterMedalGraphicRecord {
                    part: 1,
                    layer: 0,
                    graphic: "caldari.1_1".to_string(),
                    color: 16_777_215,
                }],
            }],
        );
    });
    assert_etag_and_304(&router, &spec, &medals_req);
    let medals_list = validate_and_json(&router, &spec, &medals_req, 200);
    assert_eq!(
        medals_list,
        json!([{
            "medal_id": 501,
            "title": "Star of New Eden",
            "description": "Awarded for fleet vanguard valor",
            "corporation_id": corp_id,
            "issuer_id": other_char_id,
            "date": "2023-11-14T22:13:20Z",
            "reason": "Defended staging citadel",
            "status": "public",
            "graphics": [{
                "part": 1,
                "layer": 0,
                "graphic": "caldari.1_1",
                "color": 16_777_215
            }]
        }])
    );

    // 3. GET /v2/characters/{character_id}/roles/
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/roles/")),
        401,
    );
    let roles_req = EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/roles/"))
        .with_bearer_token(&token);
    let initial_roles = validate_and_json(&router, &spec, &roles_req, 200);
    assert_eq!(
        initial_roles,
        json!({
            "roles": ["Director"],
            "roles_at_hq": [],
            "roles_at_base": [],
            "roles_at_other": []
        })
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.char_scoped_roles.insert(
            char_id,
            CharacterScopedRolesRecord {
                roles: vec!["Director".to_string(), "Personnel_Manager".to_string()],
                roles_at_hq: vec!["Hangar_Take_1".to_string(), "Config_Equipment".to_string()],
                roles_at_base: vec!["Hangar_Query_2".to_string()],
                roles_at_other: vec!["Container_Take_1".to_string()],
            },
        );
    });
    assert_etag_and_304(&router, &spec, &roles_req);
    let scoped_roles = validate_and_json(&router, &spec, &roles_req, 200);
    assert_eq!(
        scoped_roles,
        json!({
            "roles": ["Director", "Personnel_Manager"],
            "roles_at_hq": ["Hangar_Take_1", "Config_Equipment"],
            "roles_at_base": ["Hangar_Query_2"],
            "roles_at_other": ["Container_Take_1"]
        })
    );

    // 4. GET /v1/loyalty/stores/{corporation_id}/offers/
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/loyalty/stores/999999999/offers/"),
        404,
    );
    let offers_req = EsiHttpRequest::new(
        "GET",
        format!("/v1/loyalty/stores/{npc_corp_id}/offers/"),
    );
    assert_eq!(validate_and_json(&router, &spec, &offers_req, 200), json!([]));

    ctx.db.with_mut_snapshot(|snap| {
        snap.loyalty_store_offers.insert(
            npc_corp_id,
            vec![LoyaltyStoreOfferRecord {
                offer_id: 4001,
                type_id: 17_634, // Caracal Navy Issue
                quantity: 1,
                lp_cost: 50_000,
                isk_cost: 20_000_000,
                ak_cost: Some(100),
                required_items: vec![LoyaltyStoreOfferItemRecord {
                    type_id: 621, // Caracal
                    quantity: 1,
                }],
            }],
        );
    });
    assert_etag_and_304(&router, &spec, &offers_req);
    let offers = validate_and_json(&router, &spec, &offers_req, 200);
    assert_eq!(
        offers,
        json!([{
            "offer_id": 4001,
            "type_id": 17_634,
            "quantity": 1,
            "lp_cost": 50_000,
            "isk_cost": 20_000_000,
            "ak_cost": 100,
            "required_items": [{
                "type_id": 621,
                "quantity": 1
            }]
        }])
    );
}

#[tokio::test]
async fn test_esi4_wallet_char_and_corp_journal_and_transactions() {
    // parity: esi:GET /characters/{character_id}/wallet/journal, feat:esi.wallet.char-journal
    // parity: esi:GET /characters/{character_id}/wallet/transactions, feat:esi.wallet.char-transactions
    // parity: esi:GET /corporations/{corporation_id}/wallets/{division}/journal
    // parity: esi:GET /corporations/{corporation_id}/wallets/{division}/transactions
    // parity: feat:esi.wallet.corp
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/wallet/journal",
        "feat:esi.wallet.char-journal",
        "esi:GET /characters/{character_id}/wallet/transactions",
        "feat:esi.wallet.char-transactions",
        "esi:GET /corporations/{corporation_id}/wallets/{division}/journal",
        "esi:GET /corporations/{corporation_id}/wallets/{division}/transactions",
        "feat:esi.wallet.corp",
    ]);

    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let char_id = 90_000_001;
    let outsider_id = 90_000_002;
    let corp_id = 98_000_402;

    ctx.db.with_mut_snapshot(|snap| {
        snap.wallet_journal.retain(|e| e.character_id != char_id);
        snap.char_wallet_transactions.remove(&char_id);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Sovereign Treasury Corp".to_string(),
                ticker: "SVTR".to_string(),
                ceo_id: char_id,
                master_wallet: IskCents::from_isk(500_000_000),
                wallet_divisions: vec![
                    CorpWalletDivisionRecord {
                        division_id: 1,
                        name: "Master Wallet".to_string(),
                        balance: IskCents::from_isk(500_000_000),
                    },
                    CorpWalletDivisionRecord {
                        division_id: 2,
                        name: "Division 2".to_string(),
                        balance: IskCents::from_isk(125_000_000),
                    },
                ],
                members: HashSet::from([char_id]),
                member_roles: HashMap::from([(
                    char_id,
                    eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_ACCOUNTANT,
                )]),
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
    });

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-wallet.read_character_wallet.v1".to_string(),
            "esi-wallet.read_corporation_wallets.v1".to_string(),
        ],
    );
    let outsider_token = issue_token_with_scopes(
        &ctx,
        outsider_id,
        vec![
            "esi-wallet.read_character_wallet.v1".to_string(),
            "esi-wallet.read_corporation_wallets.v1".to_string(),
        ],
    );

    // 1. Character Wallet Journal (`GET /v6/characters/{character_id}/wallet/journal/`)
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v6/characters/{char_id}/wallet/journal/")),
        401,
    );
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v6/characters/{char_id}/wallet/journal/"))
            .with_bearer_token(&outsider_token),
        403,
    );
    let char_journal_req =
        EsiHttpRequest::new("GET", format!("/v6/characters/{char_id}/wallet/journal/"))
            .with_bearer_token(&token);
    assert_eq!(
        validate_and_json(&router, &spec, &char_journal_req, 200),
        json!([])
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.wallet_journal.push(WalletJournalEntry {
            entry_id: 9101,
            character_id: char_id,
            entry_type: "bounty_prizes".to_string(),
            amount: IskCents::from_isk(1_500_000),
            balance_after: IskCents::from_isk(101_500_000),
            reason: "Pirate bounty payout".to_string(),
        });
        snap.wallet_journal.push(WalletJournalEntry {
            entry_id: 9102,
            character_id: char_id,
            entry_type: "market_transaction".to_string(),
            amount: IskCents::from_isk(-250_000),
            balance_after: IskCents::from_isk(101_250_000),
            reason: "Bought Rifter hull".to_string(),
        });
        snap.wallet_journal_timestamps.insert(9101, 1_700_000_000);
        snap.wallet_journal_timestamps.insert(9102, 1_700_003_600);
    });
    let cj_resp = router.handle_request(&char_journal_req);
    assert_eq!(cj_resp.status_code, 200);
    assert_eq!(cj_resp.header("x-pages"), Some("1"));
    assert_etag_and_304(&router, &spec, &char_journal_req);
    let cj = validate_and_json(&router, &spec, &char_journal_req, 200);
    assert_eq!(
        cj,
        json!([
            {
                "id": 9102,
                "date": "2023-11-14T23:13:20Z",
                "ref_type": "market_transaction",
                "description": "Bought Rifter hull",
                "amount": -250_000.0,
                "balance": 101_250_000.0,
                "reason": "Bought Rifter hull"
            },
            {
                "id": 9101,
                "date": "2023-11-14T22:13:20Z",
                "ref_type": "bounty_prizes",
                "description": "Pirate bounty payout",
                "amount": 1_500_000.0,
                "balance": 101_500_000.0,
                "reason": "Pirate bounty payout"
            }
        ])
    );

    // 2. Character Wallet Transactions (`GET /v1/characters/{character_id}/wallet/transactions/`)
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/wallet/transactions/")),
        401,
    );
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/characters/{char_id}/wallet/transactions/?from_id=0"),
        )
        .with_bearer_token(&token),
        400,
    );
    let char_tx_req =
        EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/wallet/transactions/"))
            .with_bearer_token(&token);
    assert_eq!(validate_and_json(&router, &spec, &char_tx_req, 200), json!([]));

    ctx.db.with_mut_snapshot(|snap| {
        snap.char_wallet_transactions.insert(
            char_id,
            vec![
                WalletTransactionRecord {
                    transaction_id: 9501,
                    date_unix: 1_700_000_000,
                    location_id: 60_003_760,
                    type_id: 34,
                    unit_price: 5.0,
                    quantity: 10_000,
                    client_id: outsider_id,
                    is_buy: false,
                    is_personal: true,
                    journal_ref_id: 9101,
                },
                WalletTransactionRecord {
                    transaction_id: 9502,
                    date_unix: 1_700_003_600,
                    location_id: 60_003_760,
                    type_id: 587,
                    unit_price: 250_000.0,
                    quantity: 1,
                    client_id: outsider_id,
                    is_buy: true,
                    is_personal: true,
                    journal_ref_id: 9102,
                },
            ],
        );
    });
    assert_etag_and_304(&router, &spec, &char_tx_req);
    let char_txs = validate_and_json(&router, &spec, &char_tx_req, 200);
    assert_eq!(char_txs.as_array().unwrap().len(), 2);
    assert_eq!(char_txs[0]["transaction_id"], json!(9502));
    assert_eq!(char_txs[0]["type_id"], json!(587));
    assert_eq!(char_txs[0]["is_buy"], json!(true));
    assert_eq!(char_txs[0]["is_personal"], json!(true));

    let char_tx_filtered = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/characters/{char_id}/wallet/transactions/?from_id=9502"),
        )
        .with_bearer_token(&token),
        200,
    );
    assert_eq!(char_tx_filtered.as_array().unwrap().len(), 1);
    assert_eq!(char_tx_filtered[0]["transaction_id"], json!(9501));

    // 3. Corporation Wallets, Division Journal & Division Transactions
    let corp_wallets = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/wallets/"))
            .with_bearer_token(&token),
        200,
    );
    assert_eq!(corp_wallets.as_array().unwrap().len(), 7);
    assert_eq!(
        corp_wallets[0],
        json!({ "division": 1, "balance": 500_000_000.0 })
    );
    assert_eq!(
        corp_wallets[1],
        json!({ "division": 2, "balance": 125_000_000.0 })
    );

    // Auth & division range validation
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v4/corporations/{corp_id}/wallets/1/journal/"))
            .with_bearer_token(&outsider_token),
        403,
    );
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v4/corporations/{corp_id}/wallets/8/journal/"))
            .with_bearer_token(&token),
        400,
    );
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/corporations/{corp_id}/wallets/8/transactions/"),
        )
        .with_bearer_token(&token),
        400,
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_wallet_journals.insert(
            (corp_id, 1),
            vec![CorpWalletJournalRecord {
                id: 9801,
                date_unix: 1_700_000_000,
                ref_type: "corporation_account_withdrawal".to_string(),
                description: "SRP payout".to_string(),
                amount: -25_000_000.0,
                balance: 500_000_000.0,
                first_party_id: Some(corp_id),
                second_party_id: Some(char_id),
                reason: Some("Fleet doctrine replacement".to_string()),
            }],
        );
        snap.corp_wallet_transactions.insert(
            (corp_id, 1),
            vec![
                WalletTransactionRecord {
                    transaction_id: 9901,
                    date_unix: 1_700_000_000,
                    location_id: 60_003_760,
                    type_id: 34,
                    unit_price: 5.0,
                    quantity: 100_000,
                    client_id: outsider_id,
                    is_buy: true,
                    is_personal: false,
                    journal_ref_id: 9801,
                },
                WalletTransactionRecord {
                    transaction_id: 9902,
                    date_unix: 1_700_003_600,
                    location_id: 60_003_760,
                    type_id: 35,
                    unit_price: 12.0,
                    quantity: 50_000,
                    client_id: outsider_id,
                    is_buy: false,
                    is_personal: false,
                    journal_ref_id: 9802,
                },
            ],
        );
    });

    let corp_j_req = EsiHttpRequest::new(
        "GET",
        format!("/v4/corporations/{corp_id}/wallets/1/journal/"),
    )
    .with_bearer_token(&token);
    let corp_j_resp = router.handle_request(&corp_j_req);
    assert_eq!(corp_j_resp.status_code, 200);
    assert_eq!(corp_j_resp.header("x-pages"), Some("1"));
    assert_etag_and_304(&router, &spec, &corp_j_req);
    let corp_j = validate_and_json(&router, &spec, &corp_j_req, 200);
    assert_eq!(
        corp_j,
        json!([{
            "id": 9801,
            "date": "2023-11-14T22:13:20Z",
            "ref_type": "corporation_account_withdrawal",
            "description": "SRP payout",
            "amount": -25_000_000.0,
            "balance": 500_000_000.0,
            "first_party_id": corp_id,
            "second_party_id": char_id,
            "reason": "Fleet doctrine replacement"
        }])
    );

    let corp_tx_req = EsiHttpRequest::new(
        "GET",
        format!("/v1/corporations/{corp_id}/wallets/1/transactions/"),
    )
    .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &corp_tx_req);
    let corp_txs = validate_and_json(&router, &spec, &corp_tx_req, 200);
    assert_eq!(corp_txs.as_array().unwrap().len(), 2);
    assert_eq!(corp_txs[0]["transaction_id"], json!(9902));
    assert!(corp_txs[0].get("is_personal").is_none());

    let corp_tx_filtered = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/corporations/{corp_id}/wallets/1/transactions/?from_id=9902"),
        )
        .with_bearer_token(&token),
        200,
    );
    assert_eq!(corp_tx_filtered.as_array().unwrap().len(), 1);
    assert_eq!(corp_tx_filtered[0]["transaction_id"], json!(9901));
}
