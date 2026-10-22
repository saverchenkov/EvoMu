//! ESI-4 integration tests for Character, Assets, Calendar, Contacts, Loyalty, and Mail
//! routes backed by `SdeRegistry`, `WorldRepository`, `ServerContext.notification_records`,
//! and `EsiRuntimeState`.

#![forbid(unsafe_code)]

use eve_db::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
};
use eve_server::esi::runtime_state::{
    EsiCalendarEvent, EsiContactEntry, EsiContactNotification, EsiCorpHistoryEntry,
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
            "evomu-esi-social-mail-test",
            "https://localhost/callback",
            scopes,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    let bundle = ctx
        .db
        .exchange_pkce_code(&code, "evomu-esi-social-mail-test", PKCE_VERIFIER, now)
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
    assert!(
        resp.header("expires").is_some(),
        "Expires header must be present"
    );
    let mut cond_req = req.clone();
    cond_req
        .headers
        .insert("if-none-match".to_string(), etag);
    validate_and_json(router, spec, &cond_req, 304);
}

#[tokio::test]
async fn test_esi4_assets_char_and_corp_locations_and_names() {
    // parity: esi:GET /characters/{character_id}/assets, esi:GET /corporations/{corporation_id}/assets
    // parity: feat:esi.assets.char-assets, feat:esi.assets.char-locations, feat:esi.assets.char-names
    // parity: feat:esi.assets.corp-assets, feat:esi.assets.corp-locations-names
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/assets",
        "esi:GET /corporations/{corporation_id}/assets",
        "feat:esi.assets.char-assets",
        "feat:esi.assets.char-locations",
        "feat:esi.assets.char-names",
        "feat:esi.assets.corp-assets",
        "feat:esi.assets.corp-locations-names",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let char_id = 90_000_001;
    let other_char_id = 90_000_002;
    let corp_id = 98_000_777;
    let char_ship_id = 991_000_001;
    let char_bpc_id = 991_000_002;
    let corp_asset_id = 991_000_003;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = corp_id;
            c.active_ship_id = char_ship_id;
            c.station_id = None;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Apex Logistics Corp".to_string(),
                ticker: "APXL".to_string(),
                ceo_id: char_id,
                alliance_id: None,
                tax_rate: 0.05,
                master_wallet: IskCents::from_isk(100_000),
                members: HashSet::from([char_id]),
                member_roles: HashMap::from([(char_id, eve_db::corp_roles::ROLE_DIRECTOR)]),
                ..Default::default()
            },
        );
        snap.items.insert(
            char_ship_id,
            InventoryItemRecord {
                item_id: char_ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: 30_000_142,
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
        snap.item_positions
            .insert(char_ship_id, [12_500.0, -4_200.0, 88_000.0]);
        snap.items.insert(
            char_bpc_id,
            InventoryItemRecord {
                item_id: char_bpc_id,
                type_id: 688, // Rifter Blueprint
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: true,
                blueprint_runs_remaining: Some(10),
            },
        );
        snap.items.insert(
            corp_asset_id,
            InventoryItemRecord {
                item_id: corp_asset_id,
                type_id: 587,
                owner_id: corp_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Deliveries,
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
        snap.item_custom_names
            .insert(char_ship_id, "SCS Golden Rifter".to_string());
        snap.item_custom_names
            .insert(corp_asset_id, "Corp Flagship Rifter".to_string());
    });

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-assets.read_assets.v1".to_string(),
            "esi-assets.read_corporation_assets.v1".to_string(),
        ],
    );
    let wrong_token = issue_token_with_scopes(
        &ctx,
        other_char_id,
        vec![
            "esi-assets.read_assets.v1".to_string(),
            "esi-assets.read_corporation_assets.v1".to_string(),
        ],
    );

    // 1. Auth checks on character assets (401 without token, 403 for other character)
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v5/characters/{char_id}/assets/")),
        401,
    );
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v5/characters/{char_id}/assets/"))
            .with_bearer_token(&wrong_token),
        403,
    );

    // 2. GET /v5/characters/{character_id}/assets/ + X-Pages + ETag/304
    let char_assets_req = EsiHttpRequest::new("GET", format!("/v5/characters/{char_id}/assets/"))
        .with_bearer_token(&token);
    let char_assets_resp = router.handle_request(&char_assets_req);
    assert_eq!(char_assets_resp.status_code, 200);
    assert_eq!(char_assets_resp.header("x-pages"), Some("1"));
    assert_etag_and_304(&router, &spec, &char_assets_req);

    let char_assets = validate_and_json(&router, &spec, &char_assets_req, 200);
    let arr = char_assets.as_array().expect("character assets array");
    let ship_entry = arr
        .iter()
        .find(|a| a["item_id"] == json!(char_ship_id))
        .expect("ship asset present");
    assert_eq!(ship_entry["type_id"], json!(587));
    assert_eq!(ship_entry["location_id"], json!(30_000_142));
    assert_eq!(ship_entry["location_type"], json!("solar_system"));
    assert_eq!(ship_entry["is_singleton"], json!(true));

    let bpc_entry = arr
        .iter()
        .find(|a| a["item_id"] == json!(char_bpc_id))
        .expect("bpc asset present");
    assert_eq!(bpc_entry["type_id"], json!(688));
    assert_eq!(bpc_entry["location_id"], json!(60_003_760));
    assert_eq!(bpc_entry["location_type"], json!("station"));
    assert_eq!(bpc_entry["is_blueprint_copy"], json!(true));

    // 3. POST /v2/characters/{character_id}/assets/locations/ + POST /v1/characters/{character_id}/assets/names/
    let char_locs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", format!("/v2/characters/{char_id}/assets/locations/"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!([char_ship_id])).unwrap()),
        200,
    );
    assert_eq!(
        char_locs,
        json!([{ "item_id": char_ship_id, "position": { "x": 12_500.0, "y": -4_200.0, "z": 88_000.0 } }])
    );

    let char_names = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", format!("/v1/characters/{char_id}/assets/names/"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!([char_ship_id])).unwrap()),
        200,
    );
    assert_eq!(char_names, json!([{ "item_id": char_ship_id, "name": "SCS Golden Rifter" }]));

    // 4. GET /v5/corporations/{corporation_id}/assets/ + corp locations & names
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v5/corporations/{corp_id}/assets/"))
            .with_bearer_token(&wrong_token),
        403,
    );
    let corp_assets_req = EsiHttpRequest::new("GET", format!("/v5/corporations/{corp_id}/assets/"))
        .with_bearer_token(&token);
    let corp_assets_resp = router.handle_request(&corp_assets_req);
    assert_eq!(corp_assets_resp.status_code, 200);
    assert_eq!(corp_assets_resp.header("x-pages"), Some("1"));
    assert_etag_and_304(&router, &spec, &corp_assets_req);

    let corp_assets = validate_and_json(&router, &spec, &corp_assets_req, 200);
    assert_eq!(
        corp_assets,
        json!([{
            "is_singleton": true,
            "item_id": corp_asset_id,
            "location_flag": "Deliveries",
            "location_id": 60_003_760,
            "location_type": "station",
            "quantity": 1,
            "type_id": 587
        }])
    );

    let corp_locs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", format!("/v2/corporations/{corp_id}/assets/locations/"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!([corp_asset_id])).unwrap()),
        200,
    );
    assert_eq!(
        corp_locs,
        json!([{ "item_id": corp_asset_id, "position": { "x": 0.0, "y": 0.0, "z": 0.0 } }])
    );

    let corp_names = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", format!("/v1/corporations/{corp_id}/assets/names/"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!([corp_asset_id])).unwrap()),
        200,
    );
    assert_eq!(corp_names, json!([{ "item_id": corp_asset_id, "name": "Corp Flagship Rifter" }]));
}

#[tokio::test]
async fn test_esi4_calendar_list_event_respond_and_attendees() {
    // parity: esi:GET /characters/{character_id}/calendar, esi:GET /characters/{character_id}/calendar/{event_id}
    // parity: esi:PUT /characters/{character_id}/calendar/{event_id}, esi:GET /characters/{character_id}/calendar/{event_id}/attendees
    // parity: feat:esi.calendar.list, feat:esi.calendar.event, feat:esi.calendar.respond, feat:esi.calendar.attendees
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/calendar",
        "esi:GET /characters/{character_id}/calendar/{event_id}",
        "esi:PUT /characters/{character_id}/calendar/{event_id}",
        "esi:GET /characters/{character_id}/calendar/{event_id}/attendees",
        "feat:esi.calendar.list",
        "feat:esi.calendar.event",
        "feat:esi.calendar.respond",
        "feat:esi.calendar.attendees",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let char_id = 90_000_001;
    let other_char_id = 90_000_002;
    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-calendar.read_calendar_events.v1".to_string(),
            "esi-calendar.respond_calendar_events.v1".to_string(),
        ],
    );
    let read_only_token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec!["esi-calendar.read_calendar_events.v1".to_string()],
    );

    // 1. Auth checks & empty state
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/calendar/")),
        401,
    );
    let empty_cal = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/calendar/"))
            .with_bearer_token(&token),
        200,
    );
    assert_eq!(empty_cal, json!([]));

    // 2. Seed two calendar events in EsiRuntimeState
    {
        let mut cal = ctx.esi_state.calendar_events.write().unwrap();
        cal.insert(
            5001,
            EsiCalendarEvent {
                event_id: 5001,
                event_date_unix: 1_704_067_200, // 2024-01-01T00:00:00Z
                title: "Alliance Structure Timer".to_string(),
                text: "Formup with logistics wing".to_string(),
                duration_minutes: 120,
                importance: 1,
                owner_id: 98_000_001,
                owner_name: "Star Fleet Corp".to_string(),
                owner_type: "corporation".to_string(),
                attendees: vec![char_id, other_char_id],
            },
        );
        cal.insert(
            5002,
            EsiCalendarEvent {
                event_id: 5002,
                event_date_unix: 1_704_153_600, // 2024-01-02T00:00:00Z
                title: "Mining Op".to_string(),
                text: "Moon extraction belt".to_string(),
                duration_minutes: 60,
                importance: 0,
                owner_id: char_id,
                owner_name: "Demo Capsuleer".to_string(),
                owner_type: "character".to_string(),
                attendees: vec![char_id],
            },
        );
        let mut resp = ctx.esi_state.calendar_responses.write().unwrap();
        resp.insert((other_char_id, 5001), "tentative".to_string());
        resp.insert((char_id, 5002), "accepted".to_string());
    }

    // 3. GET /v1/characters/{character_id}/calendar/ + from_event filter + ETag/304
    let list_req = EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/calendar/"))
        .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &list_req);
    let list = validate_and_json(&router, &spec, &list_req, 200);
    assert_eq!(
        list,
        json!([
            {
                "event_date": "2024-01-01T00:00:00Z",
                "event_id": 5001,
                "event_response": "not_responded",
                "importance": 1,
                "title": "Alliance Structure Timer"
            },
            {
                "event_date": "2024-01-02T00:00:00Z",
                "event_id": 5002,
                "event_response": "accepted",
                "importance": 0,
                "title": "Mining Op"
            }
        ])
    );

    let paged_list = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/characters/{char_id}/calendar/?from_event=5002"),
        )
        .with_bearer_token(&token),
        200,
    );
    assert_eq!(paged_list.as_array().unwrap().len(), 1);
    assert_eq!(paged_list[0]["event_id"], json!(5002));

    // 4. GET /v3/characters/{character_id}/calendar/{event_id}/ (404 on unknown, 200 + ETag/304 on 5001)
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v3/characters/{char_id}/calendar/999999/"))
            .with_bearer_token(&token),
        404,
    );
    let detail_req = EsiHttpRequest::new("GET", format!("/v3/characters/{char_id}/calendar/5001/"))
        .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &detail_req);
    let detail = validate_and_json(&router, &spec, &detail_req, 200);
    assert_eq!(
        detail,
        json!({
            "date": "2024-01-01T00:00:00Z",
            "duration": 120,
            "event_id": 5001,
            "importance": 1,
            "owner_id": 98_000_001,
            "owner_name": "Star Fleet Corp",
            "owner_type": "corporation",
            "response": "not_responded",
            "text": "Formup with logistics wing",
            "title": "Alliance Structure Timer"
        })
    );

    // 5. PUT /v3/characters/{character_id}/calendar/{event_id}/ (403 without write scope, 400 invalid enum, 204 valid)
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("PUT", format!("/v3/characters/{char_id}/calendar/5001/"))
            .with_bearer_token(&read_only_token)
            .with_body(serde_json::to_vec(&json!({ "response": "accepted" })).unwrap()),
        403,
    );
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("PUT", format!("/v3/characters/{char_id}/calendar/5001/"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!({ "response": "invalid_choice" })).unwrap()),
        400,
    );
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("PUT", format!("/v3/characters/{char_id}/calendar/5001/"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!({ "response": "accepted" })).unwrap()),
        204,
    );

    // Verify mutation in event detail and attendees list
    let updated_detail = validate_and_json(&router, &spec, &detail_req, 200);
    assert_eq!(updated_detail["response"], json!("accepted"));

    let attendees_req = EsiHttpRequest::new(
        "GET",
        format!("/v1/characters/{char_id}/calendar/5001/attendees/"),
    )
    .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &attendees_req);
    let attendees = validate_and_json(&router, &spec, &attendees_req, 200);
    assert_eq!(
        attendees,
        json!([
            { "character_id": char_id, "event_response": "accepted" },
            { "character_id": other_char_id, "event_response": "tentative" }
        ])
    );
}

#[tokio::test]
async fn test_esi4_character_public_history_cspa_roles_titles() {
    // parity: esi:GET /characters/{character_id}, feat:esi.character.public
    // parity: esi:GET /characters/{character_id}/corporationhistory, feat:esi.character.corp-history
    // parity: esi:POST /characters/{character_id}/cspa, feat:esi.character.cspa
    // parity: esi:GET /characters/{character_id}/titles, feat:esi.character.titles
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}",
        "feat:esi.character.public",
        "esi:GET /characters/{character_id}/corporationhistory",
        "feat:esi.character.corp-history",
        "esi:POST /characters/{character_id}/cspa",
        "feat:esi.character.cspa",
        "esi:GET /characters/{character_id}/titles",
        "feat:esi.character.titles",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let char_id = 90_000_001;
    let stranger_id = 90_000_002;
    let corp_id = 98_000_888;
    let alliance_id = 99_000_888;

    ctx.db.with_mut_snapshot(|snap| {
        let role_mask = eve_db::corp_roles::ROLE_DIRECTOR
            | eve_db::corp_roles::ROLE_PERSONNEL_MANAGER
            | eve_db::corp_roles::ROLE_FACTORY_MANAGER;
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Vanguard Ind".to_string(),
                ticker: "VGND".to_string(),
                ceo_id: char_id,
                alliance_id: Some(alliance_id),
                tax_rate: 0.10,
                master_wallet: IskCents(0),
                members: HashSet::from([char_id]),
                member_roles: HashMap::from([(char_id, role_mask)]),
                ..Default::default()
            },
        );
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = corp_id;
            c.alliance_id = Some(alliance_id);
            c.race_id = 2; // Minmatar
            c.bloodline_id = 4; // Brutor
            c.gender = 0; // female
            c.security_status = 2.75;
            c.description = "Fleet Commander".to_string();
            c.title = "Wing Commander".to_string();
        }
        snap.character_cspa_charges
            .insert(stranger_id, IskCents::from_isk(2950));
    });
    ctx.esi_state.char_corp_history.write().unwrap().insert(
        char_id,
        vec![
            EsiCorpHistoryEntry {
                corporation_id: corp_id,
                is_deleted: false,
                record_id: 2002,
                start_date_unix: 1_700_000_000,
            },
            EsiCorpHistoryEntry {
                corporation_id: 1_000_125,
                is_deleted: true,
                record_id: 2001,
                start_date_unix: 1_680_000_000,
            },
        ],
    );
    ctx.esi_state.char_titles.write().unwrap().insert(
        char_id,
        vec![
            (1, "Wing Commander".to_string()),
            (2, "Capital Director".to_string()),
        ],
    );

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-characters.read_contacts.v1".to_string(),
            "esi-characters.read_fw_stats.v1".to_string(),
            "esi-characters.read_corporation_roles.v1".to_string(),
            "esi-characters.read_titles.v1".to_string(),
        ],
    );

    // 1. GET /v5/characters/{character_id}/ (404 on unknown, 200 + ETag/304 on char_id)
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v5/characters/999999999/"),
        404,
    );
    let pub_req = EsiHttpRequest::new("GET", format!("/v5/characters/{char_id}/"));
    assert_etag_and_304(&router, &spec, &pub_req);
    let pub_info = validate_and_json(&router, &spec, &pub_req, 200);
    assert_eq!(pub_info["corporation_id"], json!(corp_id));
    assert_eq!(pub_info["alliance_id"], json!(alliance_id));
    assert_eq!(pub_info["race_id"], json!(2));
    assert_eq!(pub_info["bloodline_id"], json!(4));
    assert_eq!(pub_info["gender"], json!("female"));
    assert_eq!(pub_info["security_status"], json!(2.75));
    assert_eq!(pub_info["description"], json!("Fleet Commander"));
    assert_eq!(pub_info["title"], json!("Wing Commander"));

    // 2. GET /v1/characters/{character_id}/corporationhistory/
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/999999999/corporationhistory/"),
        404,
    );
    let hist_req =
        EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/corporationhistory/"));
    assert_etag_and_304(&router, &spec, &hist_req);
    let hist = validate_and_json(&router, &spec, &hist_req, 200);
    assert_eq!(hist.as_array().unwrap().len(), 2);
    assert_eq!(hist[0]["corporation_id"], json!(corp_id));
    assert_eq!(hist[0]["record_id"], json!(2002));
    assert_eq!(hist[1]["corporation_id"], json!(1_000_125));
    assert_eq!(hist[1]["record_id"], json!(2001));
    assert_eq!(hist[1]["is_deleted"], json!(true));

    // 3. POST /v5/characters/{character_id}/cspa/
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", format!("/v5/characters/{char_id}/cspa/"))
            .with_body(serde_json::to_vec(&json!([stranger_id])).unwrap()),
        401,
    );
    let cspa_charge = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", format!("/v5/characters/{char_id}/cspa/"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!([stranger_id, char_id])).unwrap()),
        201,
    );
    assert_eq!(cspa_charge, json!(2950.0));

    // 4. GET /v2/characters/{character_id}/roles/
    let roles_req = EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/roles/"))
        .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &roles_req);
    let roles = validate_and_json(&router, &spec, &roles_req, 200);
    let role_list = roles["roles"].as_array().expect("roles array");
    assert!(role_list.contains(&json!("Director")));
    assert!(role_list.contains(&json!("Personnel_Manager")));
    assert!(role_list.contains(&json!("Factory_Manager")));

    // 5. GET /v1/characters/{character_id}/titles/
    let titles_req = EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/titles/"))
        .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &titles_req);
    let titles = validate_and_json(&router, &spec, &titles_req, 200);
    assert_eq!(
        titles,
        json!([
            { "title_id": 1, "name": "Wing Commander" },
            { "title_id": 2, "name": "Capital Director" }
        ])
    );
}

#[tokio::test]
async fn test_esi4_character_notifications_and_loyalty_points() {
    // parity: esi:GET /characters/{character_id}/notifications, feat:esi.character.notifications
    // parity: esi:GET /characters/{character_id}/notifications/contacts, feat:esi.character.contact-notifications
    // parity: esi:GET /characters/{character_id}/loyalty/points, feat:esi.loyalty.points
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/notifications",
        "feat:esi.character.notifications",
        "esi:GET /characters/{character_id}/notifications/contacts",
        "feat:esi.character.contact-notifications",
        "esi:GET /characters/{character_id}/loyalty/points",
        "feat:esi.loyalty.points",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let char_id = 90_000_001;
    let sender_char_id = 90_000_002;
    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-characters.read_notifications.v1".to_string(),
            "esi-characters.read_loyalty.v1".to_string(),
        ],
    );

    // 1. Empty state returns [] (no dummy fallback rows)
    let empty_notifs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v5/characters/{char_id}/notifications/"))
            .with_bearer_token(&token),
        200,
    );
    assert_eq!(empty_notifs, json!([]));

    let empty_contact_notifs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/characters/{char_id}/notifications/contacts/"),
        )
        .with_bearer_token(&token),
        200,
    );
    assert_eq!(empty_contact_notifs, json!([]));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.loyalty_points.clear();
        }
    });
    let empty_lp = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/loyalty/points/"))
            .with_bearer_token(&token),
        200,
    );
    assert_eq!(empty_lp, json!([]));

    // 2. Seed real notifications, contact notifications, and loyalty points
    let created_rec = ctx.create_character_notification(
        1_000_125,
        char_id,
        5, // AllWarDeclaredMsg
        1,
        "againstID: 99000001\ndeclaredByID: 99000002",
    );
    ctx.esi_state
        .contact_notifications
        .write()
        .unwrap()
        .insert(
            char_id,
            vec![EsiContactNotification {
                notification_id: 99_001,
                send_date_unix: 1_704_067_200,
                sender_character_id: sender_char_id,
                message: "Added you as fleet ally".to_string(),
                standing_level: 10.0,
            }],
        );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.loyalty_points.insert(1_000_125, 14_500);
            c.loyalty_points.insert(1_000_180, 3_200);
        }
    });

    let notif_req = EsiHttpRequest::new("GET", format!("/v5/characters/{char_id}/notifications/"))
        .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &notif_req);
    let notifs = validate_and_json(&router, &spec, &notif_req, 200);
    assert_eq!(notifs.as_array().unwrap().len(), 1);
    assert_eq!(notifs[0]["notification_id"], json!(created_rec.notification_id));
    assert_eq!(notifs[0]["sender_id"], json!(1_000_125));
    assert_eq!(notifs[0]["sender_type"], json!("corporation"));
    assert_eq!(notifs[0]["type"], json!("AllWarDeclaredMsg"));
    assert_eq!(
        notifs[0]["text"],
        json!("againstID: 99000001\ndeclaredByID: 99000002")
    );
    assert_eq!(notifs[0]["is_read"], json!(false));

    let cnotif_req = EsiHttpRequest::new(
        "GET",
        format!("/v1/characters/{char_id}/notifications/contacts/"),
    )
    .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &cnotif_req);
    let cnotifs = validate_and_json(&router, &spec, &cnotif_req, 200);
    assert_eq!(
        cnotifs,
        json!([{
            "message": "Added you as fleet ally",
            "notification_id": 99_001,
            "send_date": "2024-01-01T00:00:00Z",
            "sender_character_id": sender_char_id,
            "standing_level": 10.0
        }])
    );

    let lp_req = EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/loyalty/points/"))
        .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &lp_req);
    let lp = validate_and_json(&router, &spec, &lp_req, 200);
    assert_eq!(
        lp,
        json!([
            { "corporation_id": 1_000_125, "loyalty_points": 14_500 },
            { "corporation_id": 1_000_180, "loyalty_points": 3_200 }
        ])
    );
}

#[tokio::test]
async fn test_esi4_contacts_char_crud_and_labels() {
    // parity: esi:GET /characters/{character_id}/contacts, esi:POST /characters/{character_id}/contacts
    // parity: esi:PUT /characters/{character_id}/contacts, esi:DELETE /characters/{character_id}/contacts
    // parity: esi:GET /characters/{character_id}/contacts/labels
    // parity: feat:esi.contacts.char-get, feat:esi.contacts.char-write, feat:esi.contacts.char-labels
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/contacts",
        "esi:POST /characters/{character_id}/contacts",
        "esi:PUT /characters/{character_id}/contacts",
        "esi:DELETE /characters/{character_id}/contacts",
        "esi:GET /characters/{character_id}/contacts/labels",
        "feat:esi.contacts.char-get",
        "feat:esi.contacts.char-write",
        "feat:esi.contacts.char-labels",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let char_id = 90_000_001;
    let contact_char_id = 90_000_002;
    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-characters.read_contacts.v1".to_string(),
            "esi-characters.write_contacts.v1".to_string(),
        ],
    );

    ctx.esi_state
        .char_contact_labels
        .write()
        .unwrap()
        .insert(char_id, vec![(1, "Fleet Allies".to_string())]);

    // 1. GET /v1/characters/{character_id}/contacts/labels/
    let labels_req = EsiHttpRequest::new(
        "GET",
        format!("/v1/characters/{char_id}/contacts/labels/"),
    )
    .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &labels_req);
    let labels = validate_and_json(&router, &spec, &labels_req, 200);
    assert_eq!(
        labels,
        json!([{ "label_id": 1, "label_name": "Fleet Allies" }])
    );

    // 2. POST /v2/characters/{character_id}/contacts/ (400 without standing, 201 with standing/watched/label_ids)
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", format!("/v2/characters/{char_id}/contacts/"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!([contact_char_id])).unwrap()),
        400,
    );
    let created = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "POST",
            format!("/v2/characters/{char_id}/contacts/?standing=5.0&watched=true&label_ids=1"),
        )
        .with_bearer_token(&token)
        .with_body(serde_json::to_vec(&json!([contact_char_id])).unwrap()),
        201,
    );
    assert_eq!(created, json!([contact_char_id]));

    // 3. GET /v2/characters/{character_id}/contacts/ verifies persisted contact
    let get_req = EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/contacts/"))
        .with_bearer_token(&token);
    let get_resp = router.handle_request(&get_req);
    assert_eq!(get_resp.header("x-pages"), Some("1"));
    assert_etag_and_304(&router, &spec, &get_req);
    let contacts = validate_and_json(&router, &spec, &get_req, 200);
    assert_eq!(
        contacts,
        json!([{
            "contact_id": contact_char_id,
            "contact_type": "character",
            "is_blocked": false,
            "is_watched": true,
            "label_ids": [1],
            "standing": 5.0
        }])
    );

    // 4. PUT /v2/characters/{character_id}/contacts/ updates standing & watched
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "PUT",
            format!("/v2/characters/{char_id}/contacts/?standing=10.0&watched=false"),
        )
        .with_bearer_token(&token)
        .with_body(serde_json::to_vec(&json!([contact_char_id])).unwrap()),
        204,
    );
    let updated_contacts = validate_and_json(&router, &spec, &get_req, 200);
    assert_eq!(updated_contacts[0]["standing"], json!(10.0));
    assert_eq!(updated_contacts[0]["is_watched"], json!(false));

    // 5. DELETE /v2/characters/{character_id}/contacts/ removes contact
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "DELETE",
            format!("/v2/characters/{char_id}/contacts/?contact_ids={contact_char_id}"),
        )
        .with_bearer_token(&token),
        204,
    );
    let after_delete = validate_and_json(&router, &spec, &get_req, 200);
    assert_eq!(after_delete, json!([]));
}

#[tokio::test]
async fn test_esi4_contacts_corp_and_alliance() {
    // parity: esi:GET /corporations/{corporation_id}/contacts, esi:GET /corporations/{corporation_id}/contacts/labels
    // parity: esi:GET /alliances/{alliance_id}/contacts, esi:GET /alliances/{alliance_id}/contacts/labels
    // parity: feat:esi.contacts.corp, feat:esi.contacts.alliance
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/contacts",
        "esi:GET /corporations/{corporation_id}/contacts/labels",
        "esi:GET /alliances/{alliance_id}/contacts",
        "esi:GET /alliances/{alliance_id}/contacts/labels",
        "feat:esi.contacts.corp",
        "feat:esi.contacts.alliance",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let char_id = 90_000_001;
    let outsider_id = 90_000_002;
    let corp_id = 98_000_555;
    let alliance_id = 99_000_555;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = corp_id;
            c.alliance_id = Some(alliance_id);
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Coalition Vanguard".to_string(),
                ticker: "CVGD".to_string(),
                ceo_id: char_id,
                alliance_id: Some(alliance_id),
                tax_rate: 0.05,
                master_wallet: IskCents(0),
                members: HashSet::from([char_id]),
                member_roles: HashMap::from([(char_id, eve_db::corp_roles::ROLE_DIRECTOR)]),
                ..Default::default()
            },
        );
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "Northern Accord".to_string(),
                short_name: "NACC".to_string(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );
    });

    ctx.esi_state.corp_contacts.write().unwrap().insert(
        corp_id,
        vec![EsiContactEntry {
            contact_id: outsider_id,
            contact_type: "character".to_string(),
            standing: -10.0,
            is_watched: true,
            is_blocked: false,
            label_ids: vec![2],
        }],
    );
    ctx.esi_state
        .corp_contact_labels
        .write()
        .unwrap()
        .insert(corp_id, vec![(2, "Hostiles".to_string())]);

    ctx.esi_state.alliance_contacts.write().unwrap().insert(
        alliance_id,
        vec![EsiContactEntry {
            contact_id: corp_id,
            contact_type: "corporation".to_string(),
            standing: 10.0,
            is_watched: false,
            is_blocked: false,
            label_ids: vec![4],
        }],
    );
    ctx.esi_state
        .alliance_contact_labels
        .write()
        .unwrap()
        .insert(alliance_id, vec![(4, "Blue Coalition".to_string())]);

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-corporations.read_contacts.v1".to_string(),
            "esi-alliances.read_contacts.v1".to_string(),
        ],
    );
    let outsider_token = issue_token_with_scopes(
        &ctx,
        outsider_id,
        vec!["esi-corporations.read_contacts.v1".to_string()],
    );

    // 1. Membership 403 checks
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v2/corporations/{corp_id}/contacts/"))
            .with_bearer_token(&outsider_token),
        403,
    );
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v2/alliances/{alliance_id}/contacts/"))
            .with_bearer_token(&outsider_token),
        403,
    );

    // 2. Corp contacts & labels
    let corp_contacts_req =
        EsiHttpRequest::new("GET", format!("/v2/corporations/{corp_id}/contacts/"))
            .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &corp_contacts_req);
    let corp_contacts = validate_and_json(&router, &spec, &corp_contacts_req, 200);
    assert_eq!(
        corp_contacts,
        json!([{
            "contact_id": outsider_id,
            "contact_type": "character",
            "is_watched": true,
            "label_ids": [2],
            "standing": -10.0
        }])
    );

    let corp_labels_req = EsiHttpRequest::new(
        "GET",
        format!("/v1/corporations/{corp_id}/contacts/labels/"),
    )
    .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &corp_labels_req);
    let corp_labels = validate_and_json(&router, &spec, &corp_labels_req, 200);
    assert_eq!(
        corp_labels,
        json!([{ "label_id": 2, "label_name": "Hostiles" }])
    );

    // 3. Alliance contacts & labels
    let all_contacts_req =
        EsiHttpRequest::new("GET", format!("/v2/alliances/{alliance_id}/contacts/"))
            .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &all_contacts_req);
    let all_contacts = validate_and_json(&router, &spec, &all_contacts_req, 200);
    assert_eq!(
        all_contacts,
        json!([{
            "contact_id": corp_id,
            "contact_type": "corporation",
            "label_ids": [4],
            "standing": 10.0
        }])
    );

    let all_labels_req = EsiHttpRequest::new(
        "GET",
        format!("/v1/alliances/{alliance_id}/contacts/labels/"),
    )
    .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &all_labels_req);
    let all_labels = validate_and_json(&router, &spec, &all_labels_req, 200);
    assert_eq!(
        all_labels,
        json!([{ "label_id": 4, "label_name": "Blue Coalition" }])
    );
}

#[tokio::test]
async fn test_esi4_mail_send_list_read_update_delete_labels_lists() {
    // parity: esi:PUT /characters/{character_id}/mail/{mail_id}, esi:DELETE /characters/{character_id}/mail/{mail_id}
    // parity: esi:GET /characters/{character_id}/mail/labels, esi:POST /characters/{character_id}/mail/labels
    // parity: esi:DELETE /characters/{character_id}/mail/labels/{label_id}, esi:GET /characters/{character_id}/mail/lists
    // parity: feat:esi.mail.list, feat:esi.mail.send, feat:esi.mail.read-update-delete, feat:esi.mail.labels, feat:esi.mail.lists
    eve_testkit::attest(&[
        "esi:PUT /characters/{character_id}/mail/{mail_id}",
        "esi:DELETE /characters/{character_id}/mail/{mail_id}",
        "esi:GET /characters/{character_id}/mail/labels",
        "esi:POST /characters/{character_id}/mail/labels",
        "esi:DELETE /characters/{character_id}/mail/labels/{label_id}",
        "esi:GET /characters/{character_id}/mail/lists",
        "feat:esi.mail.list",
        "feat:esi.mail.send",
        "feat:esi.mail.read-update-delete",
        "feat:esi.mail.labels",
        "feat:esi.mail.lists",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let char_id = 90_000_001;
    let recipient_id = 90_000_002;
    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-mail.read_mail.v1".to_string(),
            "esi-mail.send_mail.v1".to_string(),
            "esi-mail.organize_mail.v1".to_string(),
        ],
    );
    let recipient_token = issue_token_with_scopes(
        &ctx,
        recipient_id,
        vec![
            "esi-mail.read_mail.v1".to_string(),
            "esi-mail.organize_mail.v1".to_string(),
        ],
    );

    // 1. POST /v1/characters/{character_id}/mail/ enforces CSPA charge & persists timestamp
    let initial_bal = ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = 98_000_901;
        }
        snap.character_cspa_charges
            .insert(recipient_id, IskCents::from_isk(2950));
        snap.characters.get(&char_id).unwrap().balance.to_wire_f64()
    });
    let unapproved_body = json!({
        "approved_cost": 0,
        "subject": "Capital Move Op",
        "body": "Cyno beacon online at 21:00 EVE.",
        "recipients": [{ "recipient_id": recipient_id, "recipient_type": "character" }]
    });
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", format!("/v1/characters/{char_id}/mail/"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&unapproved_body).unwrap()),
        400,
    );
    let send_body = json!({
        "approved_cost": 2950,
        "subject": "Capital Move Op",
        "body": "Cyno beacon online at 21:00 EVE.",
        "recipients": [{ "recipient_id": recipient_id, "recipient_type": "character" }]
    });
    let mail_id_val = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", format!("/v1/characters/{char_id}/mail/"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&send_body).unwrap()),
        201,
    );
    let mail_id = mail_id_val.as_i64().expect("created mail_id");
    let after_bal = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).unwrap().balance.to_wire_f64());
    assert_eq!(after_bal, initial_bal - 2950.0);

    // 2. GET /v1/characters/{recipient_id}/mail/ & GET /v1/characters/{recipient_id}/mail/{mail_id}/
    let list_req = EsiHttpRequest::new("GET", format!("/v1/characters/{recipient_id}/mail/"))
        .with_bearer_token(&recipient_token);
    assert_etag_and_304(&router, &spec, &list_req);
    let headers = validate_and_json(&router, &spec, &list_req, 200);
    let mail_hdr = headers
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["mail_id"] == json!(mail_id))
        .expect("mail in recipient inbox");
    assert_eq!(mail_hdr["subject"], json!("Capital Move Op"));
    assert_eq!(mail_hdr["from"], json!(char_id));
    assert_eq!(mail_hdr["is_read"], json!(false));

    let detail_req = EsiHttpRequest::new("GET", format!("/v1/characters/{recipient_id}/mail/{mail_id}/"))
        .with_bearer_token(&recipient_token);
    assert_etag_and_304(&router, &spec, &detail_req);
    let detail = validate_and_json(&router, &spec, &detail_req, 200);
    assert_eq!(detail["subject"], json!("Capital Move Op"));
    assert_eq!(detail["body"], json!("Cyno beacon online at 21:00 EVE."));
    assert_eq!(detail["read"], json!(false));

    // 3. Mail labels: GET /v3/, POST /v2/, DELETE /v1/ (422 on system label, 204 on custom label)
    let labels_req = EsiHttpRequest::new("GET", format!("/v3/characters/{recipient_id}/mail/labels/"))
        .with_bearer_token(&recipient_token);
    assert_etag_and_304(&router, &spec, &labels_req);
    let initial_labels = validate_and_json(&router, &spec, &labels_req, 200);
    assert!(initial_labels["labels"].as_array().unwrap().len() >= 4);

    let new_label_val = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", format!("/v2/characters/{recipient_id}/mail/labels/"))
            .with_bearer_token(&recipient_token)
            .with_body(serde_json::to_vec(&json!({ "name": "Capitals", "color": "#fe0000" })).unwrap()),
        201,
    );
    let custom_label_id = new_label_val.as_i64().expect("created label_id");

    // 4. PUT /v1/characters/{recipient_id}/mail/{mail_id}/ updates read flag and labels
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("PUT", format!("/v1/characters/{recipient_id}/mail/{mail_id}/"))
            .with_bearer_token(&recipient_token)
            .with_body(serde_json::to_vec(&json!({ "read": true, "labels": [1, custom_label_id] })).unwrap()),
        204,
    );
    let updated_detail = validate_and_json(&router, &spec, &detail_req, 200);
    assert_eq!(updated_detail["read"], json!(true));
    assert_eq!(updated_detail["labels"], json!([1, custom_label_id]));

    let updated_headers = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/characters/{recipient_id}/mail/?labels={custom_label_id}"),
        )
        .with_bearer_token(&recipient_token),
        200,
    );
    let updated_hdr = updated_headers
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["mail_id"] == json!(mail_id))
        .expect("mail in list after PUT with labels filter");
    assert_eq!(updated_hdr["is_read"], json!(true));
    assert_eq!(updated_hdr["labels"], json!([1, custom_label_id]));

    // Delete built-in label 1 -> 422, delete custom label -> 204
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("DELETE", format!("/v1/characters/{recipient_id}/mail/labels/1/"))
            .with_bearer_token(&recipient_token),
        422,
    );
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "DELETE",
            format!("/v1/characters/{recipient_id}/mail/labels/{custom_label_id}/"),
        )
        .with_bearer_token(&recipient_token),
        204,
    );

    // 5. DELETE /v1/characters/{recipient_id}/mail/{mail_id}/ removes mail
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("DELETE", format!("/v1/characters/{recipient_id}/mail/{mail_id}/"))
            .with_bearer_token(&recipient_token),
        204,
    );
    validate_and_json(&router, &spec, &detail_req, 404);

    // 6. GET /v1/characters/{character_id}/mail/lists/ (empty -> [], populated -> list)
    let lists_req = EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/mail/lists/"))
        .with_bearer_token(&token);
    let empty_lists = validate_and_json(&router, &spec, &lists_req, 200);
    assert_eq!(empty_lists, json!([]));

    ctx.esi_state.mail_lists.write().unwrap().insert(
        char_id,
        vec![(14_001, "Alliance Logistics Ping".to_string())],
    );
    assert_etag_and_304(&router, &spec, &lists_req);
    let populated_lists = validate_and_json(&router, &spec, &lists_req, 200);
    assert_eq!(
        populated_lists,
        json!([{
            "mailing_list_id": 14_001,
            "name": "Alliance Logistics Ping"
        }])
    );
}
