#![forbid(unsafe_code)]

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, OnceLock};

use eve_db::corp_roles::{ROLE_ACCOUNTANT, ROLE_DIRECTOR, ROLE_JUNIOR_ACCOUNTANT, ROLE_TRADER};
use eve_db::snapshot_ext::{
    AllianceLifecycleMeta, CalendarEventRecord, CorpWalletJournalRecord, CustomsOfficeRecord,
    FwCharacterStatsRecord, FwSystemOccupationRecord, FwWindowedStat, IncursionRecord,
    KillmailAttackerDetail, KillmailDetailRecord, LoyaltyStoreOfferItemRecord,
    LoyaltyStoreOfferRecord, MarketHistoryDayRecord, MarketOrderMeta, PlanetaryColonyLayoutRecord,
    PlanetaryLinkRecord, PlanetaryPinContentRecord, PlanetaryPinRecord, PlanetaryRouteRecord,
    SovCampaignScoreRecord, WalletTransactionRecord, WarAllyRecord, WarLifecycleMeta,
};
use eve_db::types::{
    AllianceRecord, ContractRecord, InventoryFlag, InventoryItemRecord, IskCents, JumpCloneRecord,
    KillmailRecord, MarketOrderRecord, PlanetaryColonyRecord, SavedFittingRecord, SkillQueueEntry,
    TrainedSkillRecord, UpwellStructureRecord, WalletJournalEntry, WarRecord,
};
use eve_db::WorldRepository;
use eve_machonet::SessionState;
use eve_sde::SdeRegistry;
use eve_server::esi::{EsiHttpRequest, EsiHttpResponse, EsiRouter, EsiServer, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::common::{login_pilot, pin_clock};

const DEMO_CHAR_ID: i64 = 90_000_001;
const OTHER_CHAR_ID: i64 = 90_000_002;
const THIRD_CHAR_ID: i64 = 90_000_003;
const OTHER_CORP_ID: i64 = 1_000_045;
const TEST_CORP_ID: i64 = 98_000_001;
const NPC_CORP_ID: i64 = 1_000_035; // Caldari Navy
const TEST_ALLIANCE_ID: i64 = 99_000_001;
const JITA_SYSTEM_ID: u32 = 30_000_142;
const PERIMETER_SYSTEM_ID: u32 = 30_000_144;
const AMARR_SYSTEM_ID: u32 = 30_002_187;
const THE_FORGE_REGION_ID: u32 = 10_000_002;
const JITA_STATION_ID: i64 = 60_003_760;
const TEST_STRUCTURE_ID: i64 = 1_000_000_000_001;

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| {
        Arc::new(
            SdeRegistry::open(&SdeRegistry::default_db_path())
                .expect("SDE sqlite must open for ESI integration tests"),
        )
    }))
}

fn new_ctx() -> Arc<ServerContext> {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    pin_clock(&ctx);
    Arc::new(ctx)
}

fn all_esi_scopes() -> BTreeSet<String> {
    OpenApiSpec::pinned()
        .operations
        .iter()
        .flat_map(|op| op.required_scopes.iter().cloned())
        .collect()
}

fn issue_token(
    ctx: &ServerContext,
    username: &str,
    password: &str,
    character_id: i64,
    scopes: BTreeSet<String>,
) -> String {
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            username,
            password,
            character_id,
            "evomu-cli",
            "http://localhost/callback",
            scopes.into_iter().collect(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "S256",
            now,
        )
        .expect("authorize_pkce must succeed");
    let tok = ctx
        .db
        .exchange_pkce_code(
            &code,
            "evomu-cli",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            now,
        )
        .expect("exchange_pkce_code must succeed");
    tok.access_token
}

fn seed_corp_and_characters(ctx: &ServerContext) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&DEMO_CHAR_ID) {
            ch.corporation_id = TEST_CORP_ID;
            ch.alliance_id = Some(TEST_ALLIANCE_ID);
        }
        if let Some(ch2) = snap.characters.get_mut(&OTHER_CHAR_ID) {
            ch2.corporation_id = TEST_CORP_ID;
            ch2.alliance_id = Some(TEST_ALLIANCE_ID);
        }
        // Third character outside TEST_CORP_ID / TEST_ALLIANCE_ID for non-member checks
        if let Some(mut outsider) = snap.characters.get(&OTHER_CHAR_ID).cloned() {
            outsider.character_id = THIRD_CHAR_ID;
            outsider.name = "Outsider Pilot".into();
            outsider.corporation_id = OTHER_CORP_ID;
            outsider.alliance_id = None;
            snap.characters.insert(THIRD_CHAR_ID, outsider);
        }

        if let Some(mut corp_tpl) = snap.corporations.values().next().cloned() {
            corp_tpl.corporation_id = TEST_CORP_ID;
            corp_tpl.name = "Edge Test Corp".into();
            corp_tpl.ticker = "EDGEC".into();
            corp_tpl.ceo_id = DEMO_CHAR_ID;
            corp_tpl.alliance_id = Some(TEST_ALLIANCE_ID);
            corp_tpl.members.insert(DEMO_CHAR_ID);
            corp_tpl.members.insert(OTHER_CHAR_ID);
            corp_tpl.member_roles.insert(DEMO_CHAR_ID, ROLE_DIRECTOR);
            corp_tpl.member_roles.insert(OTHER_CHAR_ID, 0);
            snap.corporations.insert(TEST_CORP_ID, corp_tpl);
        }

        snap.alliances.insert(
            TEST_ALLIANCE_ID,
            AllianceRecord {
                alliance_id: TEST_ALLIANCE_ID,
                name: "Edge Test Alliance".into(),
                short_name: "EDGEA".into(),
                executor_corp_id: TEST_CORP_ID,
                member_corp_ids: [TEST_CORP_ID].into_iter().collect(),
            },
        );
        snap.alliance_lifecycle_meta.insert(
            TEST_ALLIANCE_ID,
            AllianceLifecycleMeta {
                creator_id: DEMO_CHAR_ID,
                creator_corporation_id: TEST_CORP_ID,
                date_founded_unix: 1_700_000_000,
                faction_id: Some(500_001),
            },
        );

        snap.structures.insert(
            TEST_STRUCTURE_ID,
            UpwellStructureRecord {
                structure_id: TEST_STRUCTURE_ID,
                type_id: 35_832,
                owner_corp_id: TEST_CORP_ID,
                solar_system_id: JITA_SYSTEM_ID,
                name: "Edge Keepstar".into(),
                position: [1_000.0, 2_000.0, 3_000.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
}

fn call(
    router: &EsiRouter,
    method: &str,
    path: &str,
    token: Option<&str>,
    ip: &str,
    body: Option<&[u8]>,
) -> EsiHttpResponse {
    let mut req = EsiHttpRequest::new(method, path).with_peer_ip(ip);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    if let Some(b) = body {
        req = req.with_body(b.to_vec());
    }
    router.handle_request(&req)
}

fn assert_openapi_ok(method: &str, path: &str, resp: &EsiHttpResponse) {
    OpenApiSpec::pinned()
        .validate_response(method, path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| {
            panic!(
                "OpenAPI validation failed for {method} {path} (status {}): {e}; body={}",
                resp.status_code,
                String::from_utf8_lossy(&resp.body)
            )
        });
}

fn assert_etag_304(router: &EsiRouter, path: &str, token: Option<&str>, ip: &str) {
    let first = call(router, "GET", path, token, ip, None);
    assert_eq!(first.status_code, 200, "GET {path} expected 200");
    let etag = first
        .header("etag")
        .or_else(|| first.header("ETag"))
        .expect("GET response must include ETag header")
        .to_string();

    let mut req = EsiHttpRequest::new("GET", path)
        .with_peer_ip(ip)
        .with_header("If-None-Match", etag);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let second = router.handle_request(&req);
    assert_eq!(
        second.status_code, 304,
        "If-None-Match GET {path} expected 304, got {}",
        second.status_code
    );
    assert!(second.body.is_empty(), "304 response body must be empty");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn router_http_meta_negotiation_and_error_limit() {
    let ctx = new_ctx();
    seed_corp_and_characters(&ctx);
    let scopes = all_esi_scopes();
    let token = issue_token(&ctx, "demo", "demo-secret", DEMO_CHAR_ID, scopes);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let ip = "10.1.0.1";

    // Meta endpoints
    for path in [
        "/swagger.json",
        "/openapi.json",
        "/meta",
        "/versions",
        "/status.json",
        "/meta/compatibility-dates",
    ] {
        let resp = call(&router, "GET", path, None, ip, None);
        assert_eq!(resp.status_code, 200, "meta route {path} failed");
        assert!(!resp.body.is_empty(), "meta route {path} body empty");
    }

    // /status + OpenAPI + ETag 304
    let status_resp = call(&router, "GET", "/status", None, ip, None);
    assert_eq!(status_resp.status_code, 200);
    assert_openapi_ok("GET", "/status", &status_resp);
    assert_etag_304(&router, "/status", None, ip);

    // Compatibility date header/query validation
    let bad_compat_hdr = router.handle_request(
        &EsiHttpRequest::new("GET", "/status")
            .with_peer_ip(ip)
            .with_header("X-Compatibility-Date", "2025/99/99"),
    );
    assert_eq!(bad_compat_hdr.status_code, 400);

    let bad_compat_query = call(&router, "GET", "/status?compatibility_date=not-a-date", None, ip, None);
    assert_eq!(bad_compat_query.status_code, 400);

    let valid_compat_date = OpenApiSpec::pinned()
        .compatibility_dates
        .first()
        .expect("pinned spec must have compatibility date")
        .as_str();
    let good_compat = router.handle_request(
        &EsiHttpRequest::new("GET", "/status")
            .with_peer_ip(ip)
            .with_header("X-Compatibility-Date", valid_compat_date),
    );
    assert_eq!(good_compat.status_code, 200);

    // Datasource / X-Tenant validation
    assert_eq!(
        call(&router, "GET", "/status?datasource=singularity", None, ip, None).status_code,
        400
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", "/status")
                    .with_peer_ip(ip)
                    .with_header("X-Tenant", "invalid-tenant")
            )
            .status_code,
        400
    );
    assert_eq!(
        call(&router, "GET", "/status?datasource=tranquility", None, ip, None).status_code,
        200
    );

    // Accept-Language & ?language= negotiation on localized /insurance/prices route
    for lang in ["de", "fr", "ja"] {
        let r_hdr = router.handle_request(
            &EsiHttpRequest::new("GET", "/insurance/prices")
                .with_peer_ip(ip)
                .with_header("Accept-Language", lang),
        );
        assert_eq!(r_hdr.status_code, 200);
        assert_eq!(
            r_hdr.header("Content-Language").map(str::to_lowercase),
            Some(lang.to_string())
        );

        let r_q = call(&router, "GET", &format!("/insurance/prices?language={lang}"), None, ip, None);
        assert_eq!(r_q.status_code, 200);
        assert_eq!(
            r_q.header("Content-Language").map(str::to_lowercase),
            Some(lang.to_string())
        );
    }

    // 405 Method Not Allowed and 404 Unmatched Route
    let r405 = call(&router, "DELETE", "/status", None, ip, None);
    assert_eq!(r405.status_code, 405);

    let r404 = call(&router, "GET", "/totally/nonexistent/route", None, ip, None);
    assert_eq!(r404.status_code, 404);

    // Global ?page= validation & X-Pages (using paginated /markets/{region}/types)
    assert_eq!(call(&router, "GET", &format!("/markets/{THE_FORGE_REGION_ID}/types?page=0"), None, ip, None).status_code, 400);
    assert_eq!(call(&router, "GET", &format!("/markets/{THE_FORGE_REGION_ID}/types?page=abc"), None, ip, None).status_code, 400);
    let paged_ok = call(&router, "GET", &format!("/markets/{THE_FORGE_REGION_ID}/types?page=1"), None, ip, None);
    assert_eq!(paged_ok.status_code, 200);
    assert!(paged_ok.header("X-Pages").is_some(), "Paginated route must emit X-Pages");
    assert_eq!(call(&router, "GET", &format!("/markets/{THE_FORGE_REGION_ID}/types?page=999"), None, ip, None).status_code, 404);

    // /verify and /oauth/verify + ?token= query auth + 401/403
    assert_eq!(call(&router, "GET", "/verify", None, ip, None).status_code, 401);
    assert_eq!(call(&router, "GET", "/oauth/verify", None, ip, None).status_code, 401);
    assert_eq!(call(&router, "GET", "/verify", Some(&token), ip, None).status_code, 200);
    let q_verify = call(
        &router,
        "GET",
        &format!("/oauth/verify?token={token}"),
        None,
        ip,
        None,
    );
    assert_eq!(q_verify.status_code, 200);
    let vjson: Value = serde_json::from_slice(&q_verify.body).expect("verify json");
    assert_eq!(vjson["CharacterID"], json!(DEMO_CHAR_ID));

    // Missing scope -> 403
    let empty_scope_tok = issue_token(&ctx, "demo", "demo-secret", DEMO_CHAR_ID, BTreeSet::new());
    let wrong_scope = call(
        &router,
        "GET",
        &format!("/characters/{DEMO_CHAR_ID}/wallet"),
        Some(&empty_scope_tok),
        ip,
        None,
    );
    assert_eq!(wrong_scope.status_code, 403);

    // Character mismatch -> 403
    let mismatch = call(
        &router,
        "GET",
        &format!("/characters/{OTHER_CHAR_ID}/wallet"),
        Some(&token),
        ip,
        None,
    );
    assert_eq!(mismatch.status_code, 403);

    // Error limit 420 isolation per peer_ip
    let limited_router = EsiRouter::with_error_limit(Arc::clone(&ctx), 2, 60);
    let err_ip = "10.9.9.9";
    let clean_ip = "10.9.9.10";
    let e1 = call(&limited_router, "GET", "/no/such/1", None, err_ip, None);
    assert_eq!(e1.status_code, 404);
    assert_eq!(e1.header("X-ESI-Error-Limit-Remain"), Some("1"));
    let e2 = call(&limited_router, "GET", "/no/such/2", None, err_ip, None);
    assert_eq!(e2.status_code, 404);
    assert_eq!(e2.header("X-ESI-Error-Limit-Remain"), Some("0"));
    let e3 = call(&limited_router, "GET", "/status", None, err_ip, None);
    assert_eq!(e3.status_code, 420);
    // Clean IP unaffected
    assert_eq!(call(&limited_router, "GET", "/status", None, clean_ip, None).status_code, 200);

    // Live HTTP/1.1 TCP round-trip via EsiServer::spawn
    let (addr, _srv_router, _srv_handle) = EsiServer::spawn(Arc::clone(&ctx), "127.0.0.1:0")
        .await
        .expect("EsiServer::spawn must bind");
    let mut stream = TcpStream::connect(addr).await.expect("connect to EsiServer");
    stream
        .write_all(b"GET /status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let raw_str = String::from_utf8_lossy(&raw);
    assert!(raw_str.starts_with("HTTP/1.1 200 OK"), "unexpected HTTP line: {raw_str}");
    assert!(raw_str.contains("players"));

    // Extract ETag and verify wire-level 304 Not Modified
    let etag_val = raw_str
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if k.eq_ignore_ascii_case("etag") {
                Some(v.trim().to_string())
            } else {
                None
            }
        })
        .expect("wire HTTP /status must include ETag");
    let mut stream304 = TcpStream::connect(addr).await.unwrap();
    let req304 = format!(
        "GET /status HTTP/1.1\r\nHost: localhost\r\nIf-None-Match: {etag_val}\r\nConnection: close\r\n\r\n"
    );
    stream304.write_all(req304.as_bytes()).await.unwrap();
    let mut raw304 = Vec::new();
    stream304.read_to_end(&mut raw304).await.unwrap();
    let raw304_str = String::from_utf8_lossy(&raw304);
    assert!(
        raw304_str.starts_with("HTTP/1.1 304"),
        "expected wire 304 Not Modified, got: {raw304_str}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contracts_and_market_edge_branches() {
    let ctx = new_ctx();
    seed_corp_and_characters(&ctx);
    let now = ctx.now_unix();
    let scopes = all_esi_scopes();
    let demo_tok = issue_token(&ctx, "demo", "demo-secret", DEMO_CHAR_ID, scopes.clone());
    let plain_member_tok = issue_token(&ctx, "other", "other-secret", OTHER_CHAR_ID, scopes);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let ip = "10.1.0.2";

    let pub_active_cid = 66_001;
    let pub_expired_cid = 66_002;
    let corp_only_cid = 66_003;
    let sell_order_id = 88_001;
    let buy_order_id = 88_002;
    let struct_order_id = 88_003;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_000_050,
            InventoryItemRecord {
                item_id: 100_000_050,
                type_id: 587,
                owner_id: DEMO_CHAR_ID,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.contracts.insert(
            pub_active_cid,
            ContractRecord {
                contract_id: pub_active_cid,
                issuer_id: DEMO_CHAR_ID,
                acceptor_id: None,
                contract_type: "Auction".into(),
                status: "Outstanding".into(),
                start_station_id: JITA_STATION_ID,
                end_station_id: JITA_STATION_ID,
                price: IskCents::from_isk(100_000),
                reward: IskCents(0),
                collateral: IskCents(0),
                item_ids: vec![100_000_050],
                expiration_unix: now + 86_400,
                issuer_corp_id: TEST_CORP_ID,
                assignee_id: 0,
                availability: "Public".into(),
                buyout: IskCents::from_isk(500_000),
                current_bid: IskCents::from_isk(125_000),
                current_bidder_id: Some(OTHER_CHAR_ID),
                requested_items: vec![(34, 100)],
                bid_history: vec![(OTHER_CHAR_ID, IskCents::from_isk(125_000), now)],
                date_issued_unix: now - 300,
                title: "Public Rifter Auction".into(),
                for_corp: false,
                num_days: 7,
            },
        );
        snap.contracts.insert(
            pub_expired_cid,
            ContractRecord {
                contract_id: pub_expired_cid,
                issuer_id: DEMO_CHAR_ID,
                acceptor_id: Some(OTHER_CHAR_ID),
                contract_type: "ItemExchange".into(),
                status: "Expired".into(),
                start_station_id: JITA_STATION_ID,
                end_station_id: JITA_STATION_ID,
                price: IskCents::from_isk(50_000),
                reward: IskCents(0),
                collateral: IskCents(0),
                item_ids: vec![100_000_050],
                expiration_unix: now.saturating_sub(100),
                issuer_corp_id: TEST_CORP_ID,
                assignee_id: 0,
                availability: "Public".into(),
                buyout: IskCents(0),
                current_bid: IskCents(0),
                current_bidder_id: None,
                requested_items: Vec::new(),
                bid_history: Vec::new(),
                date_issued_unix: now - 10_000,
                title: "Expired Public Exchange".into(),
                for_corp: false,
                num_days: 1,
            },
        );
        snap.contracts.insert(
            corp_only_cid,
            ContractRecord {
                contract_id: corp_only_cid,
                issuer_id: DEMO_CHAR_ID,
                acceptor_id: None,
                contract_type: "Courier".into(),
                status: "Outstanding".into(),
                start_station_id: JITA_STATION_ID,
                end_station_id: JITA_STATION_ID,
                price: IskCents(0),
                reward: IskCents::from_isk(25_000),
                collateral: IskCents::from_isk(100_000),
                item_ids: vec![100_000_050],
                expiration_unix: now + 86_400,
                issuer_corp_id: TEST_CORP_ID,
                assignee_id: TEST_CORP_ID,
                availability: "Corporation".into(),
                buyout: IskCents(0),
                current_bid: IskCents(0),
                current_bidder_id: None,
                requested_items: Vec::new(),
                bid_history: Vec::new(),
                date_issued_unix: now - 100,
                title: "Corp Courier".into(),
                for_corp: true,
                num_days: 3,
            },
        );

        // Market orders (region sell, buy, and Upwell structure order)
        snap.market_orders.insert(
            sell_order_id,
            MarketOrderRecord {
                order_id: sell_order_id,
                owner_id: DEMO_CHAR_ID,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(6),
                vol_entered: 1_000,
                vol_remaining: 750,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_order_meta.insert(
            sell_order_id,
            MarketOrderMeta {
                issued_unix: now - 500,
                duration_days: 90,
                range: "station".into(),
                issued_by: Some(DEMO_CHAR_ID),
                wallet_division: Some(1),
                is_corporation: true,
                state: None,
            },
        );
        snap.market_orders.insert(
            buy_order_id,
            MarketOrderRecord {
                order_id: buy_order_id,
                owner_id: OTHER_CHAR_ID,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(5),
                vol_entered: 2_000,
                vol_remaining: 2_000,
                min_volume: 10,
                is_npc: false,
                escrow: IskCents::from_isk(10_000),
            },
        );
        snap.market_orders.insert(
            struct_order_id,
            MarketOrderRecord {
                order_id: struct_order_id,
                owner_id: DEMO_CHAR_ID,
                station_id: TEST_STRUCTURE_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(7),
                vol_entered: 500,
                vol_remaining: 500,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_order_meta.insert(
            struct_order_id,
            MarketOrderMeta {
                issued_unix: now - 400,
                duration_days: 90,
                range: "station".into(),
                issued_by: Some(DEMO_CHAR_ID),
                wallet_division: None,
                is_corporation: false,
                state: None,
            },
        );
        snap.market_history.insert(
            (THE_FORGE_REGION_ID, 34),
            vec![MarketHistoryDayRecord {
                date: "2026-11-01".into(),
                average: 5.5,
                highest: 6.0,
                lowest: 5.0,
                order_count: 12,
                volume: 50_000,
                open_price: Some(5.2),
                close_price: Some(5.8),
            }],
        );
    });

    // Character contracts + items + bids + errors
    let char_c = call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/contracts"), Some(&demo_tok), ip, None);
    assert_eq!(char_c.status_code, 200);
    assert_openapi_ok("GET", &format!("/characters/{DEMO_CHAR_ID}/contracts"), &char_c);
    assert_etag_304(&router, &format!("/characters/{DEMO_CHAR_ID}/contracts"), Some(&demo_tok), ip);
    assert_eq!(call(&router, "GET", "/characters/0/contracts", Some(&demo_tok), ip, None).status_code, 403);
    assert_eq!(call(&router, "GET", &format!("/characters/{OTHER_CHAR_ID}/contracts"), Some(&demo_tok), ip, None).status_code, 403);

    let c_items = call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/contracts/{pub_active_cid}/items"), Some(&demo_tok), ip, None);
    assert_eq!(c_items.status_code, 200);
    assert_openapi_ok("GET", &format!("/characters/{DEMO_CHAR_ID}/contracts/{pub_active_cid}/items"), &c_items);
    assert_eq!(call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/contracts/9999999/items"), Some(&demo_tok), ip, None).status_code, 404);
    assert_eq!(call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/contracts/0/items"), Some(&demo_tok), ip, None).status_code, 400);

    let c_bids = call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/contracts/{pub_active_cid}/bids"), Some(&demo_tok), ip, None);
    assert_eq!(c_bids.status_code, 200);
    assert_openapi_ok("GET", &format!("/characters/{DEMO_CHAR_ID}/contracts/{pub_active_cid}/bids"), &c_bids);
    assert_eq!(call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/contracts/9999999/bids"), Some(&demo_tok), ip, None).status_code, 404);

    // Public contracts
    let pub_c = call(&router, "GET", &format!("/contracts/public/{THE_FORGE_REGION_ID}"), None, ip, None);
    assert_eq!(pub_c.status_code, 200);
    assert_openapi_ok("GET", &format!("/contracts/public/{THE_FORGE_REGION_ID}"), &pub_c);
    assert_eq!(call(&router, "GET", "/contracts/public/0", None, ip, None).status_code, 400);

    let pub_items = call(&router, "GET", &format!("/contracts/public/items/{pub_active_cid}"), None, ip, None);
    assert_eq!(pub_items.status_code, 200);
    assert_openapi_ok("GET", &format!("/contracts/public/items/{pub_active_cid}"), &pub_items);
    // Expired public contract -> 204; private corp contract -> 404
    assert_eq!(call(&router, "GET", &format!("/contracts/public/items/{pub_expired_cid}"), None, ip, None).status_code, 204);
    assert_eq!(call(&router, "GET", &format!("/contracts/public/items/{corp_only_cid}"), None, ip, None).status_code, 404);

    let pub_bids = call(&router, "GET", &format!("/contracts/public/bids/{pub_active_cid}"), None, ip, None);
    assert_eq!(pub_bids.status_code, 200);
    assert_openapi_ok("GET", &format!("/contracts/public/bids/{pub_active_cid}"), &pub_bids);
    assert_eq!(call(&router, "GET", &format!("/contracts/public/bids/{pub_expired_cid}"), None, ip, None).status_code, 204);
    assert_eq!(call(&router, "GET", &format!("/contracts/public/bids/{corp_only_cid}"), None, ip, None).status_code, 404);

    // Corporation contracts (plain corp member succeeds; non-member 403; corp_id=0 -> 400)
    let corp_c = call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/contracts"), Some(&plain_member_tok), ip, None);
    assert_eq!(corp_c.status_code, 200);
    assert_openapi_ok("GET", &format!("/corporations/{TEST_CORP_ID}/contracts"), &corp_c);
    assert_eq!(call(&router, "GET", &format!("/corporations/{OTHER_CORP_ID}/contracts"), Some(&plain_member_tok), ip, None).status_code, 403);
    assert_eq!(call(&router, "GET", "/corporations/0/contracts", Some(&demo_tok), ip, None).status_code, 403);
    assert_eq!(call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/contracts/{corp_only_cid}/items"), Some(&demo_tok), ip, None).status_code, 200);
    assert_eq!(call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/contracts/{corp_only_cid}/items"), Some(&demo_tok), ip, None).status_code, 200);
    assert_eq!(call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/contracts/{corp_only_cid}/bids"), Some(&demo_tok), ip, None).status_code, 200);
    assert_eq!(call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/contracts/9999999/items"), Some(&demo_tok), ip, None).status_code, 404);

    // Markets: groups, group detail, prices, orders (all/buy/sell/type_id/invalid), history, types, structures
    let mg = call(&router, "GET", "/markets/groups", None, ip, None);
    assert_eq!(mg.status_code, 200);
    assert_openapi_ok("GET", "/markets/groups", &mg);
    let groups: Vec<i64> = serde_json::from_slice(&mg.body).unwrap();
    assert!(!groups.is_empty());
    let first_gid = groups[0];
    let mg_one = call(&router, "GET", &format!("/markets/groups/{first_gid}"), None, ip, None);
    assert_eq!(mg_one.status_code, 200);
    assert_openapi_ok("GET", &format!("/markets/groups/{first_gid}"), &mg_one);
    assert_eq!(call(&router, "GET", "/markets/groups/99999999", None, ip, None).status_code, 404);
    assert_eq!(call(&router, "GET", "/markets/groups/0", None, ip, None).status_code, 400);

    let mp = call(&router, "GET", "/markets/prices", None, ip, None);
    assert_eq!(mp.status_code, 200);
    assert_openapi_ok("GET", "/markets/prices", &mp);
    assert_etag_304(&router, "/markets/prices", None, ip);

    for ot in ["all", "buy", "sell"] {
        let path = format!("/markets/{THE_FORGE_REGION_ID}/orders?order_type={ot}&type_id=34");
        let r = call(&router, "GET", &path, None, ip, None);
        assert_eq!(r.status_code, 200);
        assert_openapi_ok("GET", &path, &r);
    }
    assert_eq!(call(&router, "GET", &format!("/markets/{THE_FORGE_REGION_ID}/orders"), None, ip, None).status_code, 400);
    assert_eq!(call(&router, "GET", &format!("/markets/{THE_FORGE_REGION_ID}/orders?order_type=bogus"), None, ip, None).status_code, 400);
    assert_eq!(call(&router, "GET", "/markets/0/orders?order_type=all", None, ip, None).status_code, 400);

    let mh = call(&router, "GET", &format!("/markets/{THE_FORGE_REGION_ID}/history?type_id=34"), None, ip, None);
    assert_eq!(mh.status_code, 200);
    assert_openapi_ok("GET", &format!("/markets/{THE_FORGE_REGION_ID}/history?type_id=34"), &mh);
    assert_eq!(call(&router, "GET", &format!("/markets/{THE_FORGE_REGION_ID}/history"), None, ip, None).status_code, 400);

    let mt = call(&router, "GET", &format!("/markets/{THE_FORGE_REGION_ID}/types"), None, ip, None);
    assert_eq!(mt.status_code, 200);
    assert_openapi_ok("GET", &format!("/markets/{THE_FORGE_REGION_ID}/types"), &mt);
    assert!(mt.header("X-Pages").is_some());

    // Structure market orders: 403 without online Standup Market Hub service, then 200 once fitted & online
    assert_eq!(
        call(&router, "GET", &format!("/markets/structures/{TEST_STRUCTURE_ID}"), Some(&demo_tok), ip, None).status_code,
        403
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&TEST_STRUCTURE_ID) {
            st.type_id = 35_834; // Keepstar supports Standup Market Hub I
        }
        snap.items.insert(
            100_000_051,
            InventoryItemRecord {
                item_id: 100_000_051,
                type_id: 35_892, // Standup Market Hub I
                owner_id: TEST_CORP_ID,
                location_id: TEST_STRUCTURE_ID,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.module_online_states.insert(100_000_051, true);
    });
    let ms = call(&router, "GET", &format!("/markets/structures/{TEST_STRUCTURE_ID}"), Some(&demo_tok), ip, None);
    assert_eq!(ms.status_code, 200);
    assert_openapi_ok("GET", &format!("/markets/structures/{TEST_STRUCTURE_ID}"), &ms);
    let ms_rows: Vec<Value> = serde_json::from_slice(&ms.body).unwrap();
    assert!(ms_rows.iter().any(|r| r["order_id"] == json!(struct_order_id)));
    assert_eq!(call(&router, "GET", &format!("/markets/structures/{TEST_STRUCTURE_ID}"), None, ip, None).status_code, 401);
    assert_eq!(call(&router, "GET", "/markets/structures/999999999999", Some(&demo_tok), ip, None).status_code, 404);
    assert_eq!(call(&router, "GET", "/markets/structures/0", Some(&demo_tok), ip, None).status_code, 400);

    // Character & Corporation orders + history + role check
    let co = call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/orders"), Some(&demo_tok), ip, None);
    assert_eq!(co.status_code, 200);
    assert_openapi_ok("GET", &format!("/characters/{DEMO_CHAR_ID}/orders"), &co);
    let coh = call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/orders/history"), Some(&demo_tok), ip, None);
    assert_eq!(coh.status_code, 200);
    assert_openapi_ok("GET", &format!("/characters/{DEMO_CHAR_ID}/orders/history"), &coh);

    // Corp orders require Accountant or Trader or Director/CEO -> plain member gets 403
    assert_eq!(call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/orders"), Some(&plain_member_tok), ip, None).status_code, 403);
    assert_eq!(call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/orders/history"), Some(&plain_member_tok), ip, None).status_code, 403);

    // Grant Trader role to OTHER_CHAR_ID and verify 200
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&TEST_CORP_ID) {
            corp.member_roles.insert(OTHER_CHAR_ID, ROLE_TRADER);
        }
    });
    let corp_o = call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/orders"), Some(&plain_member_tok), ip, None);
    assert_eq!(corp_o.status_code, 200);
    assert_openapi_ok("GET", &format!("/corporations/{TEST_CORP_ID}/orders"), &corp_o);
    let corp_oh = call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/orders/history"), Some(&plain_member_tok), ip, None);
    assert_eq!(corp_oh.status_code, 200);
    assert_openapi_ok("GET", &format!("/corporations/{TEST_CORP_ID}/orders/history"), &corp_oh);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wallet_loyalty_and_insurance_edge_branches() {
    let ctx = new_ctx();
    seed_corp_and_characters(&ctx);
    let now = ctx.now_unix();
    let scopes = all_esi_scopes();
    let demo_tok = issue_token(&ctx, "demo", "demo-secret", DEMO_CHAR_ID, scopes.clone());
    let member_tok = issue_token(&ctx, "other", "other-secret", OTHER_CHAR_ID, scopes);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let ip = "10.1.0.3";

    ctx.db.with_mut_snapshot(|snap| {
        snap.wallet_journal.push(WalletJournalEntry {
            entry_id: 99_001,
            character_id: DEMO_CHAR_ID,
            entry_type: "bounty_prizes".into(),
            amount: IskCents::from_isk(50_000),
            balance_after: IskCents::from_isk(1_050_000),
            reason: "Rat bounty".into(),
        });
        snap.wallet_journal.push(WalletJournalEntry {
            entry_id: 99_002,
            character_id: DEMO_CHAR_ID,
            entry_type: "player_donation".into(),
            amount: IskCents::from_isk(10_000),
            balance_after: IskCents::from_isk(1_060_000),
            reason: "Gift".into(),
        });
        snap.char_wallet_transactions.insert(
            DEMO_CHAR_ID,
            vec![
                WalletTransactionRecord {
                    transaction_id: 99_101,
                    date_unix: now - 60,
                    location_id: JITA_STATION_ID,
                    type_id: 34,
                    unit_price: 5.0,
                    quantity: 100,
                    client_id: OTHER_CHAR_ID,
                    is_buy: true,
                    is_personal: true,
                    journal_ref_id: 99_001,
                },
                WalletTransactionRecord {
                    transaction_id: 99_102,
                    date_unix: now,
                    location_id: JITA_STATION_ID,
                    type_id: 34,
                    unit_price: 6.0,
                    quantity: 50,
                    client_id: OTHER_CHAR_ID,
                    is_buy: false,
                    is_personal: true,
                    journal_ref_id: 99_002,
                },
            ],
        );
        snap.corp_wallet_journals.insert(
            (TEST_CORP_ID, 1),
            vec![CorpWalletJournalRecord {
                id: 99_201,
                date_unix: now,
                ref_type: "corporation_account_withdrawal".into(),
                description: "Ops".into(),
                amount: 10_000.0,
                balance: 5_000_000.0,
                first_party_id: Some(TEST_CORP_ID),
                second_party_id: Some(DEMO_CHAR_ID),
                reason: Some("Ops".into()),
            }],
        );
        snap.corp_wallet_transactions.insert(
            (TEST_CORP_ID, 1),
            vec![WalletTransactionRecord {
                transaction_id: 99_301,
                date_unix: now,
                location_id: JITA_STATION_ID,
                type_id: 34,
                unit_price: 5.5,
                quantity: 200,
                client_id: OTHER_CHAR_ID,
                is_buy: true,
                is_personal: false,
                journal_ref_id: 99_201,
            }],
        );
        if let Some(ch) = snap.characters.get_mut(&DEMO_CHAR_ID) {
            ch.loyalty_points.insert(NPC_CORP_ID, 12_500);
        }
        snap.loyalty_store_offers.insert(
            NPC_CORP_ID,
            vec![LoyaltyStoreOfferRecord {
                offer_id: 5001,
                type_id: 587,
                quantity: 1,
                lp_cost: 1_000,
                isk_cost: 500_000,
                ak_cost: Some(0),
                required_items: vec![LoyaltyStoreOfferItemRecord {
                    type_id: 34,
                    quantity: 100,
                }],
            }],
        );
    });

    // Character wallet, journal + from_id + ETag, transactions + from_id
    let cw = call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/wallet"), Some(&demo_tok), ip, None);
    assert_eq!(cw.status_code, 200);
    assert_openapi_ok("GET", &format!("/characters/{DEMO_CHAR_ID}/wallet"), &cw);
    assert_eq!(call(&router, "GET", &format!("/characters/{OTHER_CHAR_ID}/wallet"), Some(&demo_tok), ip, None).status_code, 403);

    let cwj = call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/wallet/journal?from_id=99002"), Some(&demo_tok), ip, None);
    assert_eq!(cwj.status_code, 200);
    assert_openapi_ok("GET", &format!("/characters/{DEMO_CHAR_ID}/wallet/journal?from_id=99002"), &cwj);
    assert_etag_304(&router, &format!("/characters/{DEMO_CHAR_ID}/wallet/journal"), Some(&demo_tok), ip);

    let cwt = call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/wallet/transactions?from_id=99102"), Some(&demo_tok), ip, None);
    assert_eq!(cwt.status_code, 200);
    assert_openapi_ok("GET", &format!("/characters/{DEMO_CHAR_ID}/wallet/transactions?from_id=99102"), &cwt);

    // Corporation wallets: plain member denied (403), Accountant / Junior_Accountant / Director allowed (200)
    assert_eq!(call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/wallets"), Some(&member_tok), ip, None).status_code, 403);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&TEST_CORP_ID) {
            corp.member_roles.insert(OTHER_CHAR_ID, ROLE_JUNIOR_ACCOUNTANT);
        }
    });
    let corp_w = call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/wallets"), Some(&member_tok), ip, None);
    assert_eq!(corp_w.status_code, 200);
    assert_openapi_ok("GET", &format!("/corporations/{TEST_CORP_ID}/wallets"), &corp_w);
    let divs: Vec<Value> = serde_json::from_slice(&corp_w.body).unwrap();
    assert_eq!(divs.len(), 7);

    let corp_wj = call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/wallets/1/journal"), Some(&member_tok), ip, None);
    assert_eq!(corp_wj.status_code, 200);
    assert_openapi_ok("GET", &format!("/corporations/{TEST_CORP_ID}/wallets/1/journal"), &corp_wj);
    let corp_wt = call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/wallets/1/transactions"), Some(&member_tok), ip, None);
    assert_eq!(corp_wt.status_code, 200);
    assert_openapi_ok("GET", &format!("/corporations/{TEST_CORP_ID}/wallets/1/transactions"), &corp_wt);
    assert_eq!(call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/wallets/9/journal"), Some(&demo_tok), ip, None).status_code, 400);
    assert_eq!(call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/wallets/0/transactions"), Some(&demo_tok), ip, None).status_code, 400);

    // Role upgrade check to Accountant too
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&TEST_CORP_ID) {
            corp.member_roles.insert(OTHER_CHAR_ID, ROLE_ACCOUNTANT);
        }
    });
    assert_eq!(call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/wallets"), Some(&member_tok), ip, None).status_code, 200);

    // Loyalty points & offers
    let lp = call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/loyalty/points"), Some(&demo_tok), ip, None);
    assert_eq!(lp.status_code, 200);
    assert_openapi_ok("GET", &format!("/characters/{DEMO_CHAR_ID}/loyalty/points"), &lp);
    assert_eq!(call(&router, "GET", &format!("/characters/{OTHER_CHAR_ID}/loyalty/points"), Some(&demo_tok), ip, None).status_code, 403);

    let offers = call(&router, "GET", &format!("/loyalty/stores/{NPC_CORP_ID}/offers"), None, ip, None);
    assert_eq!(offers.status_code, 200);
    assert_openapi_ok("GET", &format!("/loyalty/stores/{NPC_CORP_ID}/offers"), &offers);
    assert_eq!(call(&router, "GET", "/loyalty/stores/999999999/offers", None, ip, None).status_code, 404);

    // Insurance prices
    let ins = call(&router, "GET", "/insurance/prices", None, ip, None);
    assert_eq!(ins.status_code, 200);
    assert_openapi_ok("GET", "/insurance/prices", &ins);
    assert_etag_304(&router, "/insurance/prices", None, ip);
    let ins_de = call(&router, "GET", "/insurance/prices?language=de", None, ip, None);
    assert_eq!(ins_de.status_code, 200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skills_clones_location_ship_and_fittings_edge_branches() {
    let ctx = new_ctx();
    seed_corp_and_characters(&ctx);
    let now = ctx.now_unix();
    let scopes = all_esi_scopes();
    let demo_tok = issue_token(&ctx, "demo", "demo-secret", DEMO_CHAR_ID, scopes);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let ip = "10.1.0.4";

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(DEMO_CHAR_ID).or_default().insert(
            3402,
            TrainedSkillRecord {
                type_id: 3402,
                level: 4,
                skill_points: 45_255.0,
            },
        );
        snap.skill_queues.insert(
            DEMO_CHAR_ID,
            vec![SkillQueueEntry {
                type_id: 3402,
                target_level: 5,
                start_sp: 45_255.0,
                end_sp: 256_000.0,
                duration_seconds: 3_600.0,
                started_at_unix: Some(now.saturating_sub(100)),
            }],
        );
        snap.jump_clones.insert(
            DEMO_CHAR_ID,
            vec![JumpCloneRecord {
                jump_clone_id: 500_001,
                character_id: DEMO_CHAR_ID,
                location_id: JITA_STATION_ID,
                implants: vec![9899],
            }],
        );
        snap.active_implants.insert(DEMO_CHAR_ID, vec![9899]);
        snap.saved_fittings.insert(
            70_001,
            SavedFittingRecord {
                fitting_id: 70_001,
                owner_id: DEMO_CHAR_ID,
                name: "Seeded Rifter".into(),
                ship_type_id: 587,
                description: "Seeded".into(),
                items: vec![(2881, InventoryFlag::HiSlot0, 1)],
            },
        );
        snap.cluster_and_runtime.portraits.insert(format!("{DEMO_CHAR_ID}:64"), "ffd8ffe0".into());
    });

    // Skills, attributes, skillqueue
    for suffix in ["skills", "attributes", "skillqueue"] {
        let path = format!("/characters/{DEMO_CHAR_ID}/{suffix}");
        let r = call(&router, "GET", &path, Some(&demo_tok), ip, None);
        assert_eq!(r.status_code, 200, "GET {path}");
        assert_openapi_ok("GET", &path, &r);
        assert_etag_304(&router, &path, Some(&demo_tok), ip);
        assert_eq!(
            call(&router, "GET", &format!("/characters/{OTHER_CHAR_ID}/{suffix}"), Some(&demo_tok), ip, None).status_code,
            403
        );
    }

    // Clones & Implants
    for suffix in ["clones", "implants"] {
        let path = format!("/characters/{DEMO_CHAR_ID}/{suffix}");
        let r = call(&router, "GET", &path, Some(&demo_tok), ip, None);
        assert_eq!(r.status_code, 200, "GET {path}");
        assert_openapi_ok("GET", &path, &r);
        assert_eq!(
            call(&router, "GET", &format!("/characters/{OTHER_CHAR_ID}/{suffix}"), Some(&demo_tok), ip, None).status_code,
            403
        );
    }

    // Location, online, ship, fatigue, standings, blueprints, portrait
    for suffix in ["location", "online", "ship", "fatigue", "standings", "blueprints"] {
        let path = format!("/characters/{DEMO_CHAR_ID}/{suffix}");
        let r = call(&router, "GET", &path, Some(&demo_tok), ip, None);
        assert_eq!(r.status_code, 200, "GET {path}");
        assert_openapi_ok("GET", &path, &r);
    }
    let portrait = call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/portrait"), None, ip, None);
    assert_eq!(portrait.status_code, 200);
    assert_openapi_ok("GET", &format!("/characters/{DEMO_CHAR_ID}/portrait"), &portrait);
    assert_eq!(call(&router, "GET", "/characters/999999999/portrait", None, ip, None).status_code, 404);

    // Affiliation
    let aff_body = serde_json::to_vec(&json!([DEMO_CHAR_ID, OTHER_CHAR_ID])).unwrap();
    let aff = call(&router, "POST", "/characters/affiliation", None, ip, Some(&aff_body));
    assert_eq!(aff.status_code, 200);
    assert_openapi_ok("POST", "/characters/affiliation", &aff);
    assert_eq!(call(&router, "POST", "/characters/affiliation", None, ip, Some(b"not-json")).status_code, 400);

    // Fittings GET / POST / DELETE
    let fit_get = call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/fittings"), Some(&demo_tok), ip, None);
    assert_eq!(fit_get.status_code, 200);
    assert_openapi_ok("GET", &format!("/characters/{DEMO_CHAR_ID}/fittings"), &fit_get);

    let new_fit = serde_json::to_vec(&json!({
        "name": "API Rifter",
        "description": "Created via ESI",
        "ship_type_id": 587,
        "items": [{"type_id": 2881, "flag": "HiSlot0", "quantity": 1}]
    }))
    .unwrap();
    let fit_post = call(&router, "POST", &format!("/characters/{DEMO_CHAR_ID}/fittings"), Some(&demo_tok), ip, Some(&new_fit));
    assert_eq!(fit_post.status_code, 201);
    assert_openapi_ok("POST", &format!("/characters/{DEMO_CHAR_ID}/fittings"), &fit_post);
    let fit_created: Value = serde_json::from_slice(&fit_post.body).unwrap();
    let created_id = fit_created["fitting_id"].as_i64().unwrap();

    // Bad POST body
    assert_eq!(
        call(&router, "POST", &format!("/characters/{DEMO_CHAR_ID}/fittings"), Some(&demo_tok), ip, Some(b"{bad")).status_code,
        400
    );

    let fit_del = call(
        &router,
        "DELETE",
        &format!("/characters/{DEMO_CHAR_ID}/fittings/{created_id}"),
        Some(&demo_tok),
        ip,
        None,
    );
    assert_eq!(fit_del.status_code, 204);
    assert_eq!(
        call(&router, "DELETE", &format!("/characters/{DEMO_CHAR_ID}/fittings/{created_id}"), Some(&demo_tok), ip, None).status_code,
        404
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn assets_and_pi_edge_branches() {
    let ctx = new_ctx();
    seed_corp_and_characters(&ctx);
    let now = ctx.now_unix();
    let scopes = all_esi_scopes();
    let demo_tok = issue_token(&ctx, "demo", "demo-secret", DEMO_CHAR_ID, scopes.clone());
    let member_tok = issue_token(&ctx, "other", "other-secret", OTHER_CHAR_ID, scopes.clone());
    let outsider_tok = issue_token(&ctx, "other", "other-secret", THIRD_CHAR_ID, scopes);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let ip = "10.1.0.5";

    let char_item_id = 100_000_001;
    let corp_item_id = 100_000_099;
    let planet_id = 40_009_048;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            char_item_id,
            InventoryItemRecord {
                item_id: char_item_id,
                type_id: 587,
                owner_id: DEMO_CHAR_ID,
                location_id: JITA_SYSTEM_ID as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.item_positions.insert(char_item_id, [1_000.0, 2_000.0, 3_000.0]);
        snap.item_custom_names.insert(char_item_id, "My Edge Rifter".into());

        snap.items.insert(
            corp_item_id,
            InventoryItemRecord {
                item_id: corp_item_id,
                type_id: 587,
                owner_id: TEST_CORP_ID,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.item_custom_names.insert(corp_item_id, "Corp Rifter".into());

        snap.planetary_colonies.insert(
            (DEMO_CHAR_ID, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: DEMO_CHAR_ID,
                solar_system_id: JITA_SYSTEM_ID,
                command_center_level: 4,
                pins_count: 2,
                extracted_commodities: HashMap::from([(2073, 500)]),
                last_extracted_unix: now,
            },
        );
        snap.planetary_colony_layouts.insert(
            (DEMO_CHAR_ID, planet_id),
            PlanetaryColonyLayoutRecord {
                pins: vec![
                    PlanetaryPinRecord {
                        pin_id: 1,
                        type_id: 2254,
                        latitude: 0.1,
                        longitude: 0.2,
                        schematic_id: None,
                        install_time_unix: Some(now - 600),
                        expiry_time_unix: Some(now + 3600),
                        last_cycle_start_unix: Some(now - 60),
                        contents: vec![PlanetaryPinContentRecord { type_id: 2073, amount: 100 }],
                    },
                    PlanetaryPinRecord {
                        pin_id: 2,
                        type_id: 2493,
                        latitude: 0.15,
                        longitude: 0.25,
                        schematic_id: Some(66),
                        install_time_unix: None,
                        expiry_time_unix: None,
                        last_cycle_start_unix: None,
                        contents: Vec::new(),
                    },
                ],
                links: vec![PlanetaryLinkRecord {
                    source_pin_id: 1,
                    destination_pin_id: 2,
                    link_level: 1,
                }],
                routes: vec![PlanetaryRouteRecord {
                    route_id: 10,
                    source_pin_id: 1,
                    destination_pin_id: 2,
                    content_type_id: 2073,
                    quantity: 50,
                    waypoints: Vec::new(),
                }],
            },
        );
        snap.corp_customs_offices.insert(
            TEST_CORP_ID,
            vec![CustomsOfficeRecord {
                office_id: 1_000_000_000_111,
                system_id: JITA_SYSTEM_ID,
                reinforce_exit_start: 18,
                reinforce_exit_end: 22,
                allow_alliance_access: true,
                allow_access_with_standings: true,
                standing_level: Some("neutral".into()),
                corporation_tax_rate: Some(0.05),
                alliance_tax_rate: Some(0.08),
                excellent_standing_tax_rate: Some(0.02),
                good_standing_tax_rate: Some(0.05),
                neutral_standing_tax_rate: Some(0.10),
                bad_standing_tax_rate: Some(0.20),
                terrible_standing_tax_rate: Some(0.50),
            }],
        );
    });

    // Character assets
    let ca = call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/assets"), Some(&demo_tok), ip, None);
    assert_eq!(ca.status_code, 200);
    assert_openapi_ok("GET", &format!("/characters/{DEMO_CHAR_ID}/assets"), &ca);
    assert_etag_304(&router, &format!("/characters/{DEMO_CHAR_ID}/assets"), Some(&demo_tok), ip);
    assert_eq!(call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/assets?page=99"), Some(&demo_tok), ip, None).status_code, 404);

    let ids_body = serde_json::to_vec(&json!([char_item_id])).unwrap();
    let cal = call(&router, "POST", &format!("/characters/{DEMO_CHAR_ID}/assets/locations"), Some(&demo_tok), ip, Some(&ids_body));
    assert_eq!(cal.status_code, 200);
    assert_openapi_ok("POST", &format!("/characters/{DEMO_CHAR_ID}/assets/locations"), &cal);

    let can = call(&router, "POST", &format!("/characters/{DEMO_CHAR_ID}/assets/names"), Some(&demo_tok), ip, Some(&ids_body));
    assert_eq!(can.status_code, 200);
    assert_openapi_ok("POST", &format!("/characters/{DEMO_CHAR_ID}/assets/names"), &can);
    assert_eq!(call(&router, "POST", &format!("/characters/{DEMO_CHAR_ID}/assets/names"), Some(&demo_tok), ip, Some(b"bad")).status_code, 400);

    // Corporation assets: non-Director member -> 403, outsider -> 403, Director -> 200
    assert_eq!(call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/assets"), Some(&member_tok), ip, None).status_code, 403);
    assert_eq!(call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/assets"), Some(&outsider_tok), ip, None).status_code, 403);

    let corp_a = call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/assets"), Some(&demo_tok), ip, None);
    assert_eq!(corp_a.status_code, 200);
    assert_openapi_ok("GET", &format!("/corporations/{TEST_CORP_ID}/assets"), &corp_a);

    let corp_ids = serde_json::to_vec(&json!([corp_item_id])).unwrap();
    let corp_al = call(&router, "POST", &format!("/corporations/{TEST_CORP_ID}/assets/locations"), Some(&demo_tok), ip, Some(&corp_ids));
    assert_eq!(corp_al.status_code, 200);
    assert_openapi_ok("POST", &format!("/corporations/{TEST_CORP_ID}/assets/locations"), &corp_al);
    let corp_an = call(&router, "POST", &format!("/corporations/{TEST_CORP_ID}/assets/names"), Some(&demo_tok), ip, Some(&corp_ids));
    assert_eq!(corp_an.status_code, 200);
    assert_openapi_ok("POST", &format!("/corporations/{TEST_CORP_ID}/assets/names"), &corp_an);

    // PI planets & planet detail & customs offices
    let pl = call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/planets"), Some(&demo_tok), ip, None);
    assert_eq!(pl.status_code, 200);
    assert_openapi_ok("GET", &format!("/characters/{DEMO_CHAR_ID}/planets"), &pl);
    assert_eq!(call(&router, "GET", &format!("/characters/{OTHER_CHAR_ID}/planets"), Some(&demo_tok), ip, None).status_code, 403);

    let pd = call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/planets/{planet_id}"), Some(&demo_tok), ip, None);
    assert_eq!(pd.status_code, 200);
    assert_openapi_ok("GET", &format!("/characters/{DEMO_CHAR_ID}/planets/{planet_id}"), &pd);
    assert_eq!(call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/planets/999999999"), Some(&demo_tok), ip, None).status_code, 404);

    assert_eq!(call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/customs_offices"), Some(&member_tok), ip, None).status_code, 403);
    let poco = call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/customs_offices"), Some(&demo_tok), ip, None);
    assert_eq!(poco.status_code, 200);
    assert_openapi_ok("GET", &format!("/corporations/{TEST_CORP_ID}/customs_offices"), &poco);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sovereignty_fw_incursions_alliances_and_wars_edge_branches() {
    let ctx = new_ctx();
    seed_corp_and_characters(&ctx);
    let now = ctx.now_unix();
    let scopes = all_esi_scopes();
    let demo_tok = issue_token(&ctx, "demo", "demo-secret", DEMO_CHAR_ID, scopes.clone());
    let outsider_tok = issue_token(&ctx, "other", "other-secret", THIRD_CHAR_ID, scopes);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let ip = "10.1.0.6";

    ctx.db.with_mut_snapshot(|snap| {
        snap.incursions.insert(
            20_000_020,
            IncursionRecord {
                constellation_id: 20_000_020,
                staging_solar_system_id: JITA_SYSTEM_ID,
                infested_solar_systems: vec![JITA_SYSTEM_ID, PERIMETER_SYSTEM_ID],
                faction_id: 500_019,
                has_boss: true,
                influence: 0.75,
                state: "mobilizing".into(),
                incursion_type: "Incursion".into(),
            },
        );
        snap.sov_campaign_scores.insert(
            1,
            SovCampaignScoreRecord {
                structure_id: TEST_STRUCTURE_ID,
                attackers_score: 0.4,
                defender_score: 0.6,
            },
        );
        snap.fw_wars.push((500_001, 500_004));
        snap.fw_system_occupations.insert(
            JITA_SYSTEM_ID,
            FwSystemOccupationRecord {
                solar_system_id: JITA_SYSTEM_ID,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 1_200,
                victory_points_threshold: 3_000,
            },
        );
        snap.fw_corp_enlisted_on.insert(TEST_CORP_ID, now.saturating_sub(7_200));
        snap.fw_character_stats.insert(
            DEMO_CHAR_ID,
            FwCharacterStatsRecord {
                character_id: DEMO_CHAR_ID,
                faction_id: Some(500_001),
                enlisted_on_unix: Some(now.saturating_sub(3_600)),
                current_rank: 2,
                highest_rank: 3,
                kills: FwWindowedStat { yesterday: 1, last_week: 3, total: 8 },
                victory_points: FwWindowedStat { yesterday: 200, last_week: 600, total: 1_500 },
            },
        );
        snap.wars.insert(
            44_001,
            WarRecord {
                war_id: 44_001,
                aggressor_corp_id: TEST_CORP_ID,
                defender_corp_id: 98_000_002,
                mutual: false,
                started_unix: now.saturating_sub(500),
                active: true,
            },
        );
        snap.war_lifecycle_meta.insert(
            44_001,
            WarLifecycleMeta {
                declared_unix: Some(now.saturating_sub(600)),
                started_unix: Some(now.saturating_sub(500)),
                retracted_unix: None,
                finished_unix: None,
                open_for_allies: Some(true),
                allies: vec![WarAllyRecord {
                    corporation_id: Some(1_000_125),
                    alliance_id: None,
                }],
            },
        );
        snap.killmails.insert(
            55_001,
            KillmailRecord {
                kill_id: 55_001,
                victim_character_id: OTHER_CHAR_ID,
                attacker_character_id: DEMO_CHAR_ID,
                solar_system_id: JITA_SYSTEM_ID,
                ship_type_id: 587,
                total_value_isk: 250_000.0,
                killed_unix: now.saturating_sub(100),
                victim_corp_id: TEST_CORP_ID,
                victim_alliance_id: Some(TEST_ALLIANCE_ID),
                attacker_corp_id: TEST_CORP_ID,
                attacker_alliance_id: Some(TEST_ALLIANCE_ID),
            },
        );
    });

    // Sovereignty
    for path in ["/sovereignty/map", "/sovereignty/structures", "/sovereignty/campaigns"] {
        let r = call(&router, "GET", path, None, ip, None);
        assert_eq!(r.status_code, 200, "GET {path}");
        assert_openapi_ok("GET", path, &r);
        assert_etag_304(&router, path, None, ip);
    }

    // Incursions
    let inc = call(&router, "GET", "/incursions", None, ip, None);
    assert_eq!(inc.status_code, 200);
    assert_openapi_ok("GET", "/incursions", &inc);
    let inc_arr: Vec<Value> = serde_json::from_slice(&inc.body).unwrap();
    assert!(!inc_arr.is_empty());

    // Faction Warfare public & scoped
    for path in [
        "/fw/wars",
        "/fw/stats",
        "/fw/systems",
        "/fw/leaderboards",
        "/fw/leaderboards/characters",
        "/fw/leaderboards/corporations",
    ] {
        let r = call(&router, "GET", path, None, ip, None);
        assert_eq!(r.status_code, 200, "GET {path}");
        assert_openapi_ok("GET", path, &r);
    }
    let char_fw = call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/fw/stats"), Some(&demo_tok), ip, None);
    assert_eq!(char_fw.status_code, 200);
    assert_openapi_ok("GET", &format!("/characters/{DEMO_CHAR_ID}/fw/stats"), &char_fw);
    assert_eq!(call(&router, "GET", &format!("/characters/{OTHER_CHAR_ID}/fw/stats"), Some(&demo_tok), ip, None).status_code, 403);

    let corp_fw = call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/fw/stats"), Some(&demo_tok), ip, None);
    assert_eq!(corp_fw.status_code, 200);
    assert_openapi_ok("GET", &format!("/corporations/{TEST_CORP_ID}/fw/stats"), &corp_fw);
    assert_eq!(call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/fw/stats"), Some(&outsider_tok), ip, None).status_code, 403);

    // Alliances & Corporations
    let al_list = call(&router, "GET", "/alliances", None, ip, None);
    assert_eq!(al_list.status_code, 200);
    assert_openapi_ok("GET", "/alliances", &al_list);
    for suffix in ["", "/corporations", "/icons"] {
        let p = format!("/alliances/{TEST_ALLIANCE_ID}{suffix}");
        let r = call(&router, "GET", &p, None, ip, None);
        assert_eq!(r.status_code, 200, "GET {p}");
        assert_openapi_ok("GET", &p, &r);
        assert_eq!(call(&router, "GET", &format!("/alliances/999999999{suffix}"), None, ip, None).status_code, 404);
    }
    assert_eq!(call(&router, "GET", "/corporations/npccorps", None, ip, None).status_code, 200);
    assert_eq!(call(&router, "GET", &format!("/corporations/{NPC_CORP_ID}"), None, ip, None).status_code, 200);
    assert_eq!(call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}"), None, ip, None).status_code, 200);
    assert_eq!(call(&router, "GET", "/corporations/999999999", None, ip, None).status_code, 404);

    // Wars
    let wars = call(&router, "GET", "/wars", None, ip, None);
    assert_eq!(wars.status_code, 200);
    assert_openapi_ok("GET", "/wars", &wars);
    assert_eq!(call(&router, "GET", "/wars?max_war_id=44001", None, ip, None).status_code, 200);

    let war_one = call(&router, "GET", "/wars/44001", None, ip, None);
    assert_eq!(war_one.status_code, 200);
    assert_openapi_ok("GET", "/wars/44001", &war_one);
    assert_eq!(call(&router, "GET", "/wars/99999999", None, ip, None).status_code, 404);

    let war_km = call(&router, "GET", "/wars/44001/killmails", None, ip, None);
    assert_eq!(war_km.status_code, 200);
    assert_openapi_ok("GET", "/wars/44001/killmails", &war_km);
    assert_eq!(call(&router, "GET", "/wars/99999999/killmails", None, ip, None).status_code, 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn killmails_dogma_and_search_edge_branches() {
    let ctx = new_ctx();
    seed_corp_and_characters(&ctx);
    let now = ctx.now_unix();
    let scopes = all_esi_scopes();
    let demo_tok = issue_token(&ctx, "demo", "demo-secret", DEMO_CHAR_ID, scopes.clone());
    let member_tok = issue_token(&ctx, "other", "other-secret", OTHER_CHAR_ID, scopes);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let ip = "10.1.0.7";

    let km_id = 55_010;
    let dyn_item_id = 100_000_777;

    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(
            km_id,
            KillmailRecord {
                kill_id: km_id,
                victim_character_id: OTHER_CHAR_ID,
                attacker_character_id: DEMO_CHAR_ID,
                solar_system_id: JITA_SYSTEM_ID,
                ship_type_id: 587,
                total_value_isk: 320_000.0,
                killed_unix: now.saturating_sub(200),
                victim_corp_id: TEST_CORP_ID,
                victim_alliance_id: Some(TEST_ALLIANCE_ID),
                attacker_corp_id: TEST_CORP_ID,
                attacker_alliance_id: Some(TEST_ALLIANCE_ID),
            },
        );
        snap.killmail_details.insert(
            km_id,
            KillmailDetailRecord {
                kill_id: km_id,
                killmail_time_unix: now.saturating_sub(200),
                damage_taken: 1_234,
                victim_position: Some([10.0, 20.0, 30.0]),
                attackers: vec![KillmailAttackerDetail {
                    character_id: DEMO_CHAR_ID,
                    corporation_id: TEST_CORP_ID,
                    alliance_id: Some(TEST_ALLIANCE_ID),
                    ship_type_id: 587,
                    weapon_type_id: 2881,
                    damage_done: 1_234,
                    final_blow: true,
                    security_status: 1.5,
                }],
            },
        );
        snap.items.insert(
            dyn_item_id,
            InventoryItemRecord {
                item_id: dyn_item_id,
                type_id: 2048,
                owner_id: DEMO_CHAR_ID,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(9_u16, 15.0)]),
                source_mutaplasmid_id: Some(47_740),
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    // Character & Corp recent killmails -> extract valid hash
    let char_km = call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/killmails/recent"), Some(&demo_tok), ip, None);
    assert_eq!(char_km.status_code, 200);
    assert_openapi_ok("GET", &format!("/characters/{DEMO_CHAR_ID}/killmails/recent"), &char_km);
    let kms: Vec<Value> = serde_json::from_slice(&char_km.body).unwrap();
    let km_entry = kms.iter().find(|k| k["killmail_id"] == json!(km_id)).expect("seeded killmail must appear");
    let km_hash = km_entry["killmail_hash"].as_str().unwrap().to_string();

    assert_eq!(call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/killmails/recent"), Some(&member_tok), ip, None).status_code, 403);
    let corp_km = call(&router, "GET", &format!("/corporations/{TEST_CORP_ID}/killmails/recent"), Some(&demo_tok), ip, None);
    assert_eq!(corp_km.status_code, 200);
    assert_openapi_ok("GET", &format!("/corporations/{TEST_CORP_ID}/killmails/recent"), &corp_km);

    // Killmail detail: valid hash -> 200, wrong hash -> 422, unknown id -> 404
    let km_det = call(&router, "GET", &format!("/killmails/{km_id}/{km_hash}"), None, ip, None);
    assert_eq!(km_det.status_code, 200);
    assert_openapi_ok("GET", &format!("/killmails/{km_id}/{km_hash}"), &km_det);
    assert_eq!(call(&router, "GET", &format!("/killmails/{km_id}/deadbeefdeadbeef"), None, ip, None).status_code, 422);
    assert_eq!(call(&router, "GET", &format!("/killmails/99999999/{km_hash}"), None, ip, None).status_code, 404);

    // Dogma attributes, effects, dynamic items, universe schematics
    let da = call(&router, "GET", "/dogma/attributes", None, ip, None);
    assert_eq!(da.status_code, 200);
    let da_ids: Vec<i64> = serde_json::from_slice(&da.body).unwrap();
    let first_attr = da_ids[0];
    let da_one = call(&router, "GET", &format!("/dogma/attributes/{first_attr}"), None, ip, None);
    assert_eq!(da_one.status_code, 200);
    assert_openapi_ok("GET", &format!("/dogma/attributes/{first_attr}"), &da_one);
    assert_eq!(call(&router, "GET", "/dogma/attributes/99999999", None, ip, None).status_code, 404);

    let de = call(&router, "GET", "/dogma/effects", None, ip, None);
    assert_eq!(de.status_code, 200);
    let de_ids: Vec<i64> = serde_json::from_slice(&de.body).unwrap();
    let first_eff = de_ids[0];
    let de_one = call(&router, "GET", &format!("/dogma/effects/{first_eff}"), None, ip, None);
    assert_eq!(de_one.status_code, 200);
    assert_openapi_ok("GET", &format!("/dogma/effects/{first_eff}"), &de_one);
    assert_eq!(call(&router, "GET", "/dogma/effects/99999999", None, ip, None).status_code, 404);

    let dyn_ok = call(&router, "GET", &format!("/dogma/dynamic/items/2048/{dyn_item_id}"), None, ip, None);
    assert_eq!(dyn_ok.status_code, 200);
    assert_openapi_ok("GET", &format!("/dogma/dynamic/items/2048/{dyn_item_id}"), &dyn_ok);
    assert_eq!(call(&router, "GET", "/dogma/dynamic/items/2048/999999999", None, ip, None).status_code, 404);

    let schem = call(&router, "GET", "/universe/schematics/66", None, ip, None);
    assert_eq!(schem.status_code, 200);
    assert_openapi_ok("GET", "/universe/schematics/66", &schem);
    assert_eq!(call(&router, "GET", "/universe/schematics/99999999", None, ip, None).status_code, 404);

    // Character Search (all categories, strict=true/false, invalid inputs)
    let search_all = call(
        &router,
        "GET",
        &format!(
            "/characters/{DEMO_CHAR_ID}/search?categories=agent,alliance,character,constellation,corporation,faction,inventory_type,region,solar_system,station,structure&search=Jita&strict=false"
        ),
        Some(&demo_tok),
        ip,
        None,
    );
    assert_eq!(search_all.status_code, 200);
    assert_openapi_ok(
        "GET",
        &format!("/characters/{DEMO_CHAR_ID}/search?categories=solar_system&search=Jita"),
        &search_all,
    );
    let search_strict = call(
        &router,
        "GET",
        &format!("/characters/{DEMO_CHAR_ID}/search?categories=solar_system&search=Jita&strict=true"),
        Some(&demo_tok),
        ip,
        None,
    );
    assert_eq!(search_strict.status_code, 200);

    // Invalid search inputs
    assert_eq!(
        call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/search?categories=solar_system&search=%20%20%20"), Some(&demo_tok), ip, None).status_code,
        400
    );
    assert_eq!(
        call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/search?categories=not_a_category&search=Jita"), Some(&demo_tok), ip, None).status_code,
        400
    );
    assert_eq!(
        call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/search?search=Jita"), Some(&demo_tok), ip, None).status_code,
        400
    );
    assert_eq!(
        call(&router, "GET", &format!("/characters/{OTHER_CHAR_ID}/search?categories=solar_system&search=Jita"), Some(&demo_tok), ip, None).status_code,
        403
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn calendar_and_opportunities_edge_branches() {
    let ctx = new_ctx();
    seed_corp_and_characters(&ctx);
    let scopes = all_esi_scopes();
    let demo_tok = issue_token(&ctx, "demo", "demo-secret", DEMO_CHAR_ID, scopes.clone());
    let outsider_tok = issue_token(&ctx, "other", "other-secret", THIRD_CHAR_ID, scopes);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let ip = "10.1.0.8";

    let invited_ev_id = 80_001;
    let empty_attendees_ev_id = 80_002;

    ctx.db.with_mut_snapshot(|snap| {
        snap.calendar_events.insert(
            invited_ev_id,
            CalendarEventRecord {
                event_id: invited_ev_id,
                owner_id: DEMO_CHAR_ID,
                creator_id: DEMO_CHAR_ID,
                event_date_time: 1_760_000_000,
                duration: 60,
                importance: 1,
                title: "Fleet Formup".into(),
                description: "Bring Rifters".into(),
                scope: "character".into(),
                responses: HashMap::from([(DEMO_CHAR_ID, 2), (OTHER_CHAR_ID, 3)]),
                is_deleted: false,
            },
        );
        snap.calendar_events.insert(
            empty_attendees_ev_id,
            CalendarEventRecord {
                event_id: empty_attendees_ev_id,
                owner_id: DEMO_CHAR_ID,
                creator_id: 0,
                event_date_time: 1_760_005_000,
                duration: 30,
                importance: 0,
                title: "Private Reminder".into(),
                description: "No invitees".into(),
                scope: "character".into(),
                responses: HashMap::new(),
                is_deleted: false,
            },
        );
    });

    // Calendar summaries + from_event + detail + attendees + PUT response
    let cal_list = call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/calendar"), Some(&demo_tok), ip, None);
    assert_eq!(cal_list.status_code, 200);
    assert_openapi_ok("GET", &format!("/characters/{DEMO_CHAR_ID}/calendar"), &cal_list);
    assert_eq!(
        call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/calendar?from_event={invited_ev_id}"), Some(&demo_tok), ip, None).status_code,
        200
    );

    let ev_detail = call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/calendar/{invited_ev_id}"), Some(&demo_tok), ip, None);
    assert_eq!(ev_detail.status_code, 200);
    assert_openapi_ok("GET", &format!("/characters/{DEMO_CHAR_ID}/calendar/{invited_ev_id}"), &ev_detail);

    // Uninvited char behavior: 403 when attendees non-empty, 404 when attendees empty
    assert_eq!(
        call(&router, "GET", &format!("/characters/{THIRD_CHAR_ID}/calendar/{invited_ev_id}"), Some(&outsider_tok), ip, None).status_code,
        403
    );
    assert_eq!(
        call(&router, "GET", &format!("/characters/{THIRD_CHAR_ID}/calendar/{empty_attendees_ev_id}"), Some(&outsider_tok), ip, None).status_code,
        404
    );
    assert_eq!(
        call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/calendar/0"), Some(&demo_tok), ip, None).status_code,
        400
    );

    let att = call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/calendar/{invited_ev_id}/attendees"), Some(&demo_tok), ip, None);
    assert_eq!(att.status_code, 200);
    assert_openapi_ok("GET", &format!("/characters/{DEMO_CHAR_ID}/calendar/{invited_ev_id}/attendees"), &att);

    for resp_choice in ["accepted", "declined", "tentative"] {
        let body = serde_json::to_vec(&json!({"response": resp_choice})).unwrap();
        let put = call(
            &router,
            "PUT",
            &format!("/characters/{DEMO_CHAR_ID}/calendar/{invited_ev_id}"),
            Some(&demo_tok),
            ip,
            Some(&body),
        );
        assert_eq!(put.status_code, 204, "PUT calendar response={resp_choice}");
    }
    let bad_put = serde_json::to_vec(&json!({"response": "maybe"})).unwrap();
    assert_eq!(
        call(&router, "PUT", &format!("/characters/{DEMO_CHAR_ID}/calendar/{invited_ev_id}"), Some(&demo_tok), ip, Some(&bad_put)).status_code,
        400
    );

    // Opportunities (legacy routes: /opportunities/groups, /opportunities/tasks, /characters/{id}/opportunities)
    let og = call(&router, "GET", "/opportunities/groups", None, ip, None);
    assert_eq!(og.status_code, 200);
    assert_openapi_ok("GET", "/opportunities/groups", &og);
    let group_ids: Vec<i64> = serde_json::from_slice(&og.body).unwrap();
    assert!(!group_ids.is_empty());
    assert_eq!(call(&router, "GET", &format!("/opportunities/groups/{}", group_ids[0]), None, ip, None).status_code, 404);

    let ot = call(&router, "GET", "/opportunities/tasks", None, ip, None);
    assert_eq!(ot.status_code, 200);
    assert_openapi_ok("GET", "/opportunities/tasks", &ot);
    let task_ids: Vec<i64> = serde_json::from_slice(&ot.body).unwrap();
    assert!(!task_ids.is_empty());
    assert_eq!(call(&router, "GET", &format!("/opportunities/tasks/{}", task_ids[0]), None, ip, None).status_code, 404);

    let char_opps = call(&router, "GET", &format!("/characters/{DEMO_CHAR_ID}/opportunities"), Some(&demo_tok), ip, None);
    assert_eq!(char_opps.status_code, 200);
    assert_openapi_ok("GET", &format!("/characters/{DEMO_CHAR_ID}/opportunities"), &char_opps);
    assert_eq!(
        call(&router, "GET", &format!("/characters/{OTHER_CHAR_ID}/opportunities"), Some(&demo_tok), ip, None).status_code,
        403
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn routes_and_ui_edge_branches() {
    let ctx = new_ctx();
    seed_corp_and_characters(&ctx);
    let scopes = all_esi_scopes();
    let demo_tok = issue_token(&ctx, "demo", "demo-secret", DEMO_CHAR_ID, scopes);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let ip = "10.1.0.9";

    // Route calculation across flags, avoid, waypoints, connections, error branches
    for flag in ["shortest", "secure", "insecure"] {
        let p = format!("/route/{JITA_SYSTEM_ID}/{AMARR_SYSTEM_ID}?flag={flag}");
        let r = call(&router, "GET", &p, None, ip, None);
        assert_eq!(r.status_code, 200, "GET {p}");
        assert_openapi_ok("GET", &p, &r);
        let hops: Vec<u32> = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(hops.first().copied(), Some(JITA_SYSTEM_ID));
        assert_eq!(hops.last().copied(), Some(AMARR_SYSTEM_ID));
    }

    // Waypoint inclusion + custom direct connection Jita|Amarr
    let wp = call(
        &router,
        "GET",
        &format!("/route/{JITA_SYSTEM_ID}/{PERIMETER_SYSTEM_ID}?waypoints={PERIMETER_SYSTEM_ID}"),
        None,
        ip,
        None,
    );
    assert_eq!(wp.status_code, 200);

    let conn = call(
        &router,
        "GET",
        &format!("/route/{JITA_SYSTEM_ID}/{AMARR_SYSTEM_ID}?connections={JITA_SYSTEM_ID}|{AMARR_SYSTEM_ID}"),
        None,
        ip,
        None,
    );
    assert_eq!(conn.status_code, 200);
    let conn_hops: Vec<u32> = serde_json::from_slice(&conn.body).unwrap();
    assert_eq!(conn_hops, vec![JITA_SYSTEM_ID, AMARR_SYSTEM_ID]);

    // Invalid flag, unknown system, unreachable when avoiding all Jita exits
    assert_eq!(
        call(&router, "GET", &format!("/route/{JITA_SYSTEM_ID}/{AMARR_SYSTEM_ID}?flag=warp"), None, ip, None).status_code,
        400
    );
    assert_eq!(
        call(&router, "GET", "/route/99999999/30002187", None, ip, None).status_code,
        404
    );
    // Avoid Jita's connected neighbors -> verify unreachable 404
    let jita_neighbors: Vec<u32> = ctx
        .sde
        .stargates()
        .values()
        .filter(|g| g.from_solar_system_id == JITA_SYSTEM_ID)
        .map(|g| g.to_solar_system_id)
        .collect();
    let avoid_csv = jita_neighbors
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        call(
            &router,
            "GET",
            &format!("/route/{JITA_SYSTEM_ID}/{AMARR_SYSTEM_ID}?avoid={avoid_csv}"),
            None,
            ip,
            None
        )
        .status_code,
        404
    );

    // UI endpoints while character is OFFLINE
    assert_eq!(
        call(
            &router,
            "POST",
            &format!("/ui/autopilot/waypoint?destination_id={JITA_SYSTEM_ID}&add_to_beginning=false&clear_other_waypoints=false"),
            Some(&demo_tok),
            ip,
            None,
        )
        .status_code,
        501
    );
    assert_eq!(
        call(
            &router,
            "POST",
            &format!("/ui/openwindow/information?target_id={DEMO_CHAR_ID}"),
            Some(&demo_tok),
            ip,
            None,
        )
        .status_code,
        400
    );

    // Log character ONLINE and exercise /ui/* endpoints
    let mut sess = SessionState::new(76_001);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    assert_eq!(
        call(
            &router,
            "POST",
            &format!("/ui/autopilot/waypoint?destination_id={PERIMETER_SYSTEM_ID}&add_to_beginning=false&clear_other_waypoints=true"),
            Some(&demo_tok),
            ip,
            None,
        )
        .status_code,
        204
    );
    assert_eq!(
        call(
            &router,
            "POST",
            &format!("/ui/openwindow/information?target_id={DEMO_CHAR_ID}"),
            Some(&demo_tok),
            ip,
            None,
        )
        .status_code,
        204
    );
    // Non-owner type_id -> 501 per ui_routes/fleet_routes design
    assert_eq!(
        call(
            &router,
            "POST",
            "/ui/openwindow/information?target_id=587",
            Some(&demo_tok),
            ip,
            None,
        )
        .status_code,
        501
    );
    assert_eq!(
        call(
            &router,
            "POST",
            "/ui/openwindow/marketdetails?type_id=34",
            Some(&demo_tok),
            ip,
            None,
        )
        .status_code,
        204
    );
    let newmail = serde_json::to_vec(&json!({
        "subject": "Hello",
        "body": "Test mail",
        "recipients": [OTHER_CHAR_ID]
    }))
    .unwrap();
    assert_eq!(
        call(&router, "POST", "/ui/openwindow/newmail", Some(&demo_tok), ip, Some(&newmail)).status_code,
        204
    );
    assert_eq!(
        call(&router, "POST", "/ui/openwindow/newmail", Some(&demo_tok), ip, Some(b"{}")).status_code,
        400
    );
}
