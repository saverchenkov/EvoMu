//! Integration tests for `ESI-7` Remote Client UI endpoints (`POST /ui/autopilot/waypoint`,
//! `POST /ui/openwindow/{contract,information,marketdetails,newmail}`), Character & Corporation
//! Bookmarks (`feat:esi.bookmarks.char`, `feat:esi.bookmarks.corp`), Character Location/Ship/Online
//! (`feat:esi.location.online`), Character Skills/Queue/Attributes (`feat:esi.skills.queue`,
//! `feat:esi.skills.attributes`), Legacy Opportunities (`feat:esi.opportunities.legacy`), and
//! overall `plan:ESI-7` closure.

#![forbid(unsafe_code)]

use eve_db::{
    BookmarkFolderRecord, BookmarkRecord, ContractRecord, CorporationRecord, InventoryFlag,
    InventoryItemRecord, IskCents, NpeProgressRecord, SkillQueueEntry, TrainedSkillRecord,
    UpwellStructureRecord,
};
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token_with_scopes(ctx: &ServerContext, character_id: i64, scopes: &[&str]) -> String {
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&character_id) {
            ch.user_id = 1;
        }
        Ok(())
    });
    let now = ctx.now_unix();
    let scope_vec = scopes.iter().map(|s| (*s).to_string()).collect();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            character_id,
            "evomu-esi-ui-features-test",
            "https://localhost/callback",
            scope_vec,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-esi-ui-features-test", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

fn call(router: &EsiRouter, req: EsiHttpRequest, expected: u16) -> Value {
    let spec = OpenApiSpec::pinned();
    let resp = router.handle_request(&req);
    let raw_path = req.raw_target.split('?').next().unwrap().to_string();
    assert_eq!(
        resp.status_code,
        expected,
        "{} {} -> {} ({})",
        req.method,
        req.raw_target,
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
    // Validate against pinned OpenAPI spec for non-legacy routes
    if !raw_path.contains("/bookmarks") && !raw_path.contains("/opportunities") {
        spec.validate_response(&req.method, &raw_path, resp.status_code, &resp.headers, &resp.body)
            .unwrap_or_else(|e| panic!("OpenAPI validation failed for {}: {e}", req.raw_target));
    }
    if expected == 204 {
        assert!(resp.body.is_empty(), "204 No Content must have empty body");
        return Value::Null;
    }
    let body = resp.json().expect("JSON body");
    if expected == 200 {
        let etag = resp.header("ETag").expect("ETag").to_string();
        let cc = resp.header("Cache-Control").expect("Cache-Control");
        assert!(
            cc.contains("max-age="),
            "expected max-age in Cache-Control: {cc}"
        );
        assert!(resp.header("Expires").is_some() && resp.header("Last-Modified").is_some());
        let again = router.handle_request(&req.clone().with_header("If-None-Match", etag.clone()));
        assert_eq!(again.status_code, 304, "conditional GET with matching ETag");
        assert!(again.body.is_empty());
        assert_eq!(again.header("ETag"), Some(etag.as_str()));
    } else if expected >= 400 {
        assert!(
            body["error"].as_str().is_some_and(|s| !s.is_empty()),
            "ESI error model"
        );
    }
    body
}

fn setup_ui_and_features_env() -> (Arc<ServerContext>, EsiRouter) {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Apex Horizon Syndicate".to_string(),
                ticker: "APEX".to_string(),
                ceo_id: char_id,
                alliance_id: None,
                tax_rate: 0.05,
                master_wallet: IskCents::from_isk(5_000_000),
                members: HashSet::from([char_id, 90_000_002]),
                ..Default::default()
            },
        );

        // Active ship for character 90000001: Raven (type_id 638)
        let ship_item_id = 100_000_501_i64;
        snap.items.insert(
            ship_item_id,
            InventoryItemRecord {
                item_id: ship_item_id,
                type_id: 638,
                owner_id: char_id,
                location_id: 60_003_760,
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
        snap.item_custom_names
            .insert(ship_item_id, "ISS Sovereign".to_string());

        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = 1;
            ch.corporation_id = corp_id;
            ch.solar_system_id = 30_000_142;
            ch.station_id = Some(60_003_760);
            ch.structure_id = None;
            ch.active_ship_id = ship_item_id;
            ch.clone_grade = 2;
            ch.skill_points = 53_255.0;
            ch.free_skill_points = 15_000.0;
            ch.neural.charisma = 19;
            ch.neural.intelligence = 25;
            ch.neural.memory = 21;
            ch.neural.perception = 27;
            ch.neural.willpower = 23;
            ch.neural.bonus_remaps = 2;
            ch.neural.last_remap_unix = Some(now.saturating_sub(3600));
            ch.neural.remap_cooldown_until_unix = Some(now + 86_400);
        }

        let mut skill_map = HashMap::new();
        skill_map.insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 4,
                skill_points: 45_255.0,
            },
        );
        skill_map.insert(
            3301,
            TrainedSkillRecord {
                type_id: 3301,
                level: 3,
                skill_points: 8_000.0,
            },
        );
        snap.skills.insert(char_id, skill_map);

        snap.skill_queues.insert(
            char_id,
            vec![
                SkillQueueEntry {
                    type_id: 3300,
                    target_level: 5,
                    start_sp: 45_255.0,
                    end_sp: 256_000.0,
                    duration_seconds: 7_200.0,
                    started_at_unix: Some(now.saturating_sub(600)),
                },
                SkillQueueEntry {
                    type_id: 3301,
                    target_level: 4,
                    start_sp: 8_000.0,
                    end_sp: 45_255.0,
                    duration_seconds: 10_800.0,
                    started_at_unix: None,
                },
            ],
        );

        // Seed an Upwell structure and a Contract for UI waypoint/contract tests
        snap.structures.insert(
            1_000_000_099_001,
            UpwellStructureRecord {
                structure_id: 1_000_000_099_001,
                type_id: 35_832, // Astrahus
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Jita Trade Citadel".to_string(),
                position: [100_000.0, 200_000.0, 300_000.0],
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 5000.0,
                armor_hp: 5000.0,
                hull_hp: 5000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
        snap.contracts.insert(
            700_001,
            ContractRecord {
                contract_id: 700_001,
                issuer_id: char_id,
                acceptor_id: None,
                contract_type: "ItemExchange".to_string(),
                status: "Outstanding".to_string(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                price: IskCents::from_isk(180_000_000),
                reward: IskCents(0),
                collateral: IskCents(0),
                item_ids: Vec::new(),
                expiration_unix: now + 86_400,
                issuer_corp_id: corp_id,
                for_corp: false,
                availability: "Public".to_string(),
                assignee_id: 0,
                title: "Raven Hull Deal".to_string(),
                num_days: 0,
                buyout: IskCents(0),
                current_bid: IskCents(0),
                current_bidder_id: None,
                requested_items: Vec::new(),
                bid_history: Vec::new(),
                date_issued_unix: now.saturating_sub(1_800),
            },
        );
    });
    let router = EsiRouter::new(Arc::clone(&ctx));
    (ctx, router)
}

#[tokio::test]
async fn test_esi_ui_autopilot_waypoint_and_open_windows() {
    // parity: esi:POST /ui/autopilot/waypoint
    // parity: esi:POST /ui/openwindow/contract, esi:POST /ui/openwindow/information
    // parity: esi:POST /ui/openwindow/marketdetails, esi:POST /ui/openwindow/newmail
    // parity: feat:esi.ui.autopilot, feat:esi.ui.open-windows, feat:navigation.route-waypoints
    eve_testkit::attest(&[
        "esi:POST /ui/autopilot/waypoint",
        "esi:POST /ui/openwindow/contract",
        "esi:POST /ui/openwindow/information",
        "esi:POST /ui/openwindow/marketdetails",
        "esi:POST /ui/openwindow/newmail",
        "feat:esi.ui.autopilot",
        "feat:esi.ui.open-windows",
        "feat:navigation.route-waypoints",
    ]);

    let (ctx, router) = setup_ui_and_features_env();
    let char_id = 90_000_001_i64;
    let user_id = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).unwrap().user_id);

    let ui_token = issue_token_with_scopes(
        &ctx,
        char_id,
        &["esi-ui.write_waypoint.v1", "esi-ui.open_window.v1"],
    );
    let no_scope_token = issue_token_with_scopes(&ctx, char_id, &[]);

    // 1. Auth & scope enforcement on all 5 UI endpoints (401 without token, 403 without scope)
    for path in [
        "/v2/ui/autopilot/waypoint/?add_to_beginning=false&clear_other_waypoints=false&destination_id=30000142",
        "/v1/ui/openwindow/contract/?contract_id=700001",
        "/v1/ui/openwindow/information/?target_id=90000001",
        "/v1/ui/openwindow/marketdetails/?type_id=34",
        "/v1/ui/openwindow/newmail/",
    ] {
        call(&router, EsiHttpRequest::new("POST", path), 401);
        call(
            &router,
            EsiHttpRequest::new("POST", path).with_bearer_token(&no_scope_token),
            403,
        );
    }

    // 2. Offline session checks:
    // - /ui/autopilot/waypoint returns 501 when no live client session is online
    // - /ui/openwindow/* return 400 when character has no active online session
    call(
        &router,
        EsiHttpRequest::new(
            "POST",
            "/v2/ui/autopilot/waypoint/?add_to_beginning=false&clear_other_waypoints=false&destination_id=30000142",
        )
        .with_bearer_token(&ui_token),
        501,
    );
    call(
        &router,
        EsiHttpRequest::new("POST", "/v1/ui/openwindow/contract/?contract_id=700001")
            .with_bearer_token(&ui_token),
        400,
    );

    // 3. Bring character online and clear any prior notifications
    let _ = ctx.drain_notifications_for_char(char_id);
    assert!(ctx
        .db
        .acquire_online_lock(user_id, 5001, Some(char_id), false, ctx.now_unix())
        .is_ok());

    // 4. Waypoint append/prepend/clear; the only store is CharacterRecord.autopilot_waypoints (D-6)
    let waypoints = || ctx.db.with_snapshot(|s| s.characters[&char_id].autopilot_waypoints.clone());
    call(
        &router,
        EsiHttpRequest::new(
            "POST",
            "/v2/ui/autopilot/waypoint/?add_to_beginning=false&clear_other_waypoints=false&destination_id=30000142",
        )
        .with_bearer_token(&ui_token),
        204,
    );
    call(
        &router,
        EsiHttpRequest::new(
            "POST",
            "/v2/ui/autopilot/waypoint/?add_to_beginning=false&clear_other_waypoints=false&destination_id=60003760",
        )
        .with_bearer_token(&ui_token),
        204,
    );
    assert_eq!(
        waypoints(),
        vec![30_000_142, 60_003_760]
    );

    // Prepend Upwell structure 1_000_000_099_001 with add_to_beginning=true
    call(
        &router,
        EsiHttpRequest::new(
            "POST",
            "/v2/ui/autopilot/waypoint/?add_to_beginning=true&clear_other_waypoints=false&destination_id=1000000099001",
        )
        .with_bearer_token(&ui_token),
        204,
    );
    assert_eq!(
        waypoints(),
        vec![1_000_000_099_001, 30_000_142, 60_003_760]
    );

    // Replace queue with clear_other_waypoints=true (Amarr 30002187)
    call(
        &router,
        EsiHttpRequest::new(
            "POST",
            "/v2/ui/autopilot/waypoint/?add_to_beginning=false&clear_other_waypoints=true&destination_id=30002187",
        )
        .with_bearer_token(&ui_token),
        204,
    );
    assert_eq!(
        waypoints(),
        vec![30_002_187]
    );

    // Negative autopilot waypoint checks: unknown destination -> 404, invalid query params -> 400
    call(
        &router,
        EsiHttpRequest::new(
            "POST",
            "/v2/ui/autopilot/waypoint/?add_to_beginning=false&clear_other_waypoints=false&destination_id=999999999",
        )
        .with_bearer_token(&ui_token),
        404,
    );
    call(
        &router,
        EsiHttpRequest::new(
            "POST",
            "/v2/ui/autopilot/waypoint/?add_to_beginning=notabool&clear_other_waypoints=false&destination_id=30000142",
        )
        .with_bearer_token(&ui_token),
        400,
    );

    // 5. OpenWindow endpoints: contract, information, marketdetails, newmail
    call(
        &router,
        EsiHttpRequest::new("POST", "/v1/ui/openwindow/contract/?contract_id=700001")
            .with_bearer_token(&ui_token),
        204,
    );
    call(
        &router,
        EsiHttpRequest::new("POST", "/v1/ui/openwindow/contract/?contract_id=999999")
            .with_bearer_token(&ui_token),
        404,
    );
    call(
        &router,
        EsiHttpRequest::new("POST", "/v1/ui/openwindow/contract/?contract_id=-5")
            .with_bearer_token(&ui_token),
        400,
    );

    call(
        &router,
        EsiHttpRequest::new("POST", "/v1/ui/openwindow/information/?target_id=90000002")
            .with_bearer_token(&ui_token),
        204,
    );
    // Non-owner target (solar system): no V24 client push opens its info window -> 501
    call(
        &router,
        EsiHttpRequest::new("POST", "/v1/ui/openwindow/information/?target_id=30000142")
            .with_bearer_token(&ui_token),
        501,
    );
    call(
        &router,
        EsiHttpRequest::new("POST", "/v1/ui/openwindow/information/?target_id=0")
            .with_bearer_token(&ui_token),
        400,
    );

    call(
        &router,
        EsiHttpRequest::new("POST", "/v1/ui/openwindow/marketdetails/?type_id=34")
            .with_bearer_token(&ui_token),
        204,
    );
    call(
        &router,
        EsiHttpRequest::new("POST", "/v1/ui/openwindow/marketdetails/?type_id=99999999")
            .with_bearer_token(&ui_token),
        404,
    );

    call(
        &router,
        EsiHttpRequest::new("POST", "/v1/ui/openwindow/newmail/")
            .with_bearer_token(&ui_token)
            .with_body(
                serde_json::to_vec(&json!({
                    "subject": "Fleet Op Tonight",
                    "body": "Formup at 20:00 EVE in Jita 4-4.",
                    "recipients": [90_000_002],
                }))
                .unwrap(),
            ),
        204,
    );
    // Negative newmail checks: empty recipients -> 400, both to_corp_or_alliance_id and to_mailing_list_id -> 422
    call(
        &router,
        EsiHttpRequest::new("POST", "/v1/ui/openwindow/newmail/")
            .with_bearer_token(&ui_token)
            .with_body(
                serde_json::to_vec(&json!({
                    "subject": "Bad Mail",
                    "body": "No recipients",
                    "recipients": [],
                }))
                .unwrap(),
            ),
        400,
    );
    call(
        &router,
        EsiHttpRequest::new("POST", "/v1/ui/openwindow/newmail/")
            .with_bearer_token(&ui_token)
            .with_body(
                serde_json::to_vec(&json!({
                    "subject": "Conflicting Targets",
                    "body": "Both corp and mailing list set",
                    "recipients": [90_000_002],
                    "to_corp_or_alliance_id": 98_000_001,
                    "to_mailing_list_id": 1001,
                }))
                .unwrap(),
            ),
        422,
    );

    assert_eq!(waypoints(), vec![30_002_187]);

    let drained = ctx.drain_notifications_for_char(char_id);
    let args_of = |event: &str| -> Vec<Vec<PyRep>> {
        drained.iter().filter(|n| n.event == event).map(|n| n.args.clone()).collect()
    };

    // V24 notification payloads (client_expectations.json arities)
    assert_eq!(args_of("OnAutopilotUpdated"), vec![Vec::<PyRep>::new(); 4]);
    assert_eq!(args_of("OnShowContract"), vec![vec![PyRep::Integer(700_001)]]);
    assert_eq!(args_of("OnShowOwnerDetailsWindow"), vec![vec![PyRep::Integer(90_000_002)]]);
    assert_eq!(args_of("OnShowMarketDetails"), vec![vec![PyRep::Integer(34)]]);
    let mail = args_of("OnCreateNewMailWindow");
    assert!(mail.len() == 1 && mail[0].len() == 5);
    assert_eq!(mail[0][0], PyRep::List(vec![PyRep::Integer(90_000_002)]));
    assert_eq!(mail[0][3], PyRep::String("Fleet Op Tonight".into()));

    // Invented notifications must never be pushed over the MachoNet wire
    assert!(args_of("OnDestinationSet").is_empty());
    assert!(args_of("OnWindowOpened").is_empty());
}

#[tokio::test]
async fn test_esi_bookmarks_location_skills_opportunities_and_plan_esi_7() {
    // parity: feat:esi.bookmarks.char, feat:esi.bookmarks.corp
    // parity: feat:esi.location.online
    // parity: feat:esi.skills.queue, feat:esi.skills.attributes
    // parity: feat:esi.opportunities.legacy
    eve_testkit::attest(&[
        "feat:esi.bookmarks.char",
        "feat:esi.bookmarks.corp",
        "feat:esi.location.online",
        "feat:esi.skills.queue",
        "feat:esi.skills.attributes",
        "feat:esi.opportunities.legacy",
    ]);

    let (ctx, router) = setup_ui_and_features_env();
    let char_id = 90_000_001_i64;
    let other_char_id = 90_000_002_i64;
    let corp_id = 98_000_001_i64;
    let user_id = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).unwrap().user_id);

    let full_token = issue_token_with_scopes(
        &ctx,
        char_id,
        &[
            "esi-bookmarks.read_character_bookmarks.v1",
            "esi-bookmarks.read_corporation_bookmarks.v1",
            "esi-location.read_location.v1",
            "esi-location.read_ship_type.v1",
            "esi-location.read_online.v1",
            "esi-skills.read_skills.v1",
            "esi-skills.read_skillqueue.v1",
            "esi-characters.read_opportunities.v1",
        ],
    );
    let other_token = issue_token_with_scopes(
        &ctx,
        other_char_id,
        &[
            "esi-bookmarks.read_character_bookmarks.v1",
            "esi-location.read_location.v1",
            "esi-skills.read_skills.v1",
        ],
    );

    // 1. Character & Corporation Bookmarks + Folders (`feat:esi.bookmarks.char`, `feat:esi.bookmarks.corp`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmark_folders.insert(
            10,
            BookmarkFolderRecord {
                folder_id: 10,
                owner_id: char_id,
                folder_name: "Safe Spots".to_string(),
                is_shared: false,
                corporation_id: None,
                ..Default::default()
            },
        );
        snap.bookmark_folders.insert(
            20,
            BookmarkFolderRecord {
                folder_id: 20,
                owner_id: corp_id,
                folder_name: "Corp Undock Perches".to_string(),
                is_shared: true,
                corporation_id: Some(corp_id),
                ..Default::default()
            },
        );
        snap.bookmarks.insert(
            5001,
            BookmarkRecord {
                bookmark_id: 5001,
                owner_id: char_id,
                label: "Jita Deep Safe".to_string(),
                solar_system_id: 30_000_142,
                x: 1.5e11,
                y: -2.0e10,
                z: 4.2e11,
                item_id: None,
                memo: "Midpoint warp spot".to_string(),
                folder_id: Some(10),
                is_shared: false,
            },
        );
        snap.bookmarks.insert(
            5002,
            BookmarkRecord {
                bookmark_id: 5002,
                owner_id: corp_id,
                label: "4-4 Insta Undock".to_string(),
                solar_system_id: 30_000_142,
                x: 10_000.0,
                y: 20_000.0,
                z: 30_000.0,
                item_id: None,
                memo: "Zero-degree alignment".to_string(),
                folder_id: Some(20),
                is_shared: true,
            },
        );
    });

    let char_bms = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/bookmarks/"))
            .with_bearer_token(&full_token),
        200,
    );
    let char_bms_arr = char_bms.as_array().unwrap();
    assert_eq!(char_bms_arr.len(), 1);
    assert_eq!(char_bms_arr[0]["bookmark_id"], json!(5001));
    assert_eq!(char_bms_arr[0]["label"], json!("Jita Deep Safe"));
    assert_eq!(char_bms_arr[0]["notes"], json!("Midpoint warp spot"));
    assert_eq!(char_bms_arr[0]["folder_id"], json!(10));
    assert_eq!(char_bms_arr[0]["location_id"], json!(30_000_142));

    let char_folders = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/bookmarks/folders/"))
            .with_bearer_token(&full_token),
        200,
    );
    assert_eq!(
        char_folders,
        json!([{ "folder_id": 10, "name": "Safe Spots" }])
    );

    // Cross-character bookmark access returns 403
    call(
        &router,
        EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/bookmarks/"))
            .with_bearer_token(&other_token),
        403,
    );

    let corp_bms = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/bookmarks/"))
            .with_bearer_token(&full_token),
        200,
    );
    let corp_bms_arr = corp_bms.as_array().unwrap();
    assert!(corp_bms_arr.iter().any(|b| {
        b["bookmark_id"] == json!(5002)
            && b["label"] == json!("4-4 Insta Undock")
            && b["notes"] == json!("Zero-degree alignment")
            && b["folder_id"] == json!(20)
    }));

    let corp_folders = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/bookmarks/folders/"))
            .with_bearer_token(&full_token),
        200,
    );
    assert_eq!(
        corp_folders,
        json!([{
            "folder_id": 20,
            "name": "Corp Undock Perches",
            "creator_id": corp_id,
        }])
    );

    // 2. Character Location, Ship, and Online Status (`feat:esi.location.online`)
    let loc = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/location/"))
            .with_bearer_token(&full_token),
        200,
    );
    assert_eq!(
        loc,
        json!({
            "solar_system_id": 30_000_142,
            "station_id": 60_003_760,
        })
    );

    let ship = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/ship/"))
            .with_bearer_token(&full_token),
        200,
    );
    assert_eq!(
        ship,
        json!({
            "ship_type_id": 638,
            "ship_item_id": 100_000_501,
            "ship_name": "ISS Sovereign",
        })
    );

    let online_before = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/online/"))
            .with_bearer_token(&full_token),
        200,
    );
    assert_eq!(online_before["online"], json!(false));

    assert!(ctx
        .db
        .acquire_online_lock(user_id, 6001, Some(char_id), false, ctx.now_unix())
        .is_ok());
    let online_active = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/online/"))
            .with_bearer_token(&full_token),
        200,
    );
    assert_eq!(online_active["online"], json!(true));
    assert!(online_active["logins"].as_i64().unwrap() >= 1);

    ctx.db.release_online_lock(user_id, 6001);
    let online_after = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/online/"))
            .with_bearer_token(&full_token),
        200,
    );
    assert_eq!(online_after["online"], json!(false));

    // 3. Character Attributes, Skills, and Skill Queue (`feat:esi.skills.attributes`, `feat:esi.skills.queue`)
    let attrs = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/attributes/"))
            .with_bearer_token(&full_token),
        200,
    );
    assert_eq!(attrs["charisma"], json!(19));
    assert_eq!(attrs["intelligence"], json!(25));
    assert_eq!(attrs["memory"], json!(21));
    assert_eq!(attrs["perception"], json!(27));
    assert_eq!(attrs["willpower"], json!(23));
    assert_eq!(attrs["bonus_remaps"], json!(2));
    assert!(attrs["last_remap_date"].is_string());
    assert!(attrs["accrued_remap_cooldown_date"].is_string());

    let skills_resp = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v4/characters/{char_id}/skills/"))
            .with_bearer_token(&full_token),
        200,
    );
    assert_eq!(skills_resp["total_sp"], json!(53_255));
    assert_eq!(skills_resp["unallocated_sp"], json!(15_000));
    assert_eq!(skills_resp["skills"].as_array().unwrap().len(), 2);

    let queue_resp = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/skillqueue/"))
            .with_bearer_token(&full_token),
        200,
    );
    let queue_arr = queue_resp.as_array().unwrap();
    assert_eq!(queue_arr.len(), 2);
    assert_eq!(queue_arr[0]["skill_id"], json!(3300));
    assert_eq!(queue_arr[0]["finished_level"], json!(5));
    assert_eq!(queue_arr[0]["queue_position"], json!(0));
    assert!(queue_arr[0]["start_date"].is_string());
    assert!(queue_arr[0]["finish_date"].is_string());
    assert_eq!(queue_arr[1]["skill_id"], json!(3301));
    assert_eq!(queue_arr[1]["finished_level"], json!(4));
    assert_eq!(queue_arr[1]["queue_position"], json!(1));

    // 4. Legacy Opportunities (`feat:esi.opportunities.legacy`)
    let opp_groups = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/opportunities/groups/"),
        200,
    );
    let group_ids = opp_groups.as_array().unwrap();
    assert!(!group_ids.is_empty());

    let opp_tasks = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/opportunities/tasks/"),
        200,
    );
    let task_ids = opp_tasks.as_array().unwrap();
    assert!(!task_ids.is_empty());
    let first_tid = task_ids[0].as_i64().unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.npe_progress.insert(
            char_id,
            NpeProgressRecord {
                character_id: char_id,
                node_graph_id: 473,
                current_node_id: 1,
                completed: false,
                career_goal_progress: HashMap::new(),
                claimed_goals: HashSet::from([first_tid as u32]),
            },
        );
    });

    let char_opps = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/opportunities/"))
            .with_bearer_token(&full_token),
        200,
    );
    let char_opps_arr = char_opps.as_array().unwrap();
    assert_eq!(char_opps_arr.len(), 1);
    assert_eq!(char_opps_arr[0]["task_id"], json!(first_tid));
}
