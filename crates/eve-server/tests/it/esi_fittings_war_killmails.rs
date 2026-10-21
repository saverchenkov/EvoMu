//! `esi_fittings_war_killmails` — `ESI-4`/`ESI-3` closure tests for the saved-fitting write
//! endpoints (`POST /characters/{character_id}/fittings`, `DELETE
//! /characters/{character_id}/fittings/{fitting_id}`) and the public `GET /wars/{war_id}/killmails`
//! list. Every response is validated against `OpenApiSpec::pinned()` (status, required headers,
//! body schema); values come from the real SDE and from `WorldRepository` state, and the fitting
//! writes are read back through the client's own `charFittingMgr.GetFittings` RPC.

use eve_db::{
    CorporationRecord, InventoryFlag, IskCents, KillmailRecord, SavedFittingRecord, WarRecord,
};
use eve_proto::PyRep;
use eve_server::esi::sample_routes::compute_killmail_hash;
use eve_server::esi::{EsiHttpRequest, EsiHttpResponse, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
const PILOT: i64 = 90_000_001;
const OTHER_PILOT: i64 = 90_000_002;

fn token_with_scopes(ctx: &ServerContext, character_id: i64, scopes: &[&str]) -> String {
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
            "evomu-esi-fittings-wars",
            "https://localhost/callback",
            scopes.iter().map(|s| s.to_string()).collect(),
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-esi-fittings-wars", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

/// Sends `req`, asserts the exact status, validates status/headers/body against the pinned
/// OpenAPI document and returns the raw response.
fn send(router: &EsiRouter, req: &EsiHttpRequest, status: u16) -> EsiHttpResponse {
    let resp = router.handle_request(req);
    assert_eq!(
        resp.status_code,
        status,
        "{} {} -> {}",
        req.method,
        req.raw_target,
        String::from_utf8_lossy(&resp.body)
    );
    let path = req.raw_target.split('?').next().unwrap_or(&req.raw_target);
    OpenApiSpec::pinned()
        .validate_response(
            &req.method,
            path,
            resp.status_code,
            &resp.headers,
            &resp.body,
        )
        .unwrap_or_else(|e| panic!("pinned OpenAPI validation {} {}: {e}", req.method, path));
    resp
}

/// Error-model check: pinned `Error` schema body `{"error": <msg>, "status": <code>}`.
fn assert_error(resp: &EsiHttpResponse, status: u16, needle: &str) {
    let body = resp.json().expect("error JSON");
    assert_eq!(body["status"], json!(status));
    let msg = body["error"].as_str().expect("error message");
    assert!(
        msg.contains(needle),
        "error {msg:?} must contain {needle:?}"
    );
}

fn fitting_body(name: &str, ship: u32, items: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "name": name,
        "description": "ESI write test",
        "ship_type_id": ship,
        "items": items,
    }))
    .unwrap()
}

/// Rows of the client's `charFittingMgr.GetFittings` rowset as
/// `(fittingID, name, shipTypeID, [(typeID, flag, qty)])`.
async fn client_fittings(ctx: &ServerContext) -> Vec<(i64, String, i64, Vec<(i64, i64, i64)>)> {
    let mut sess = eve_machonet::SessionState::new(91_001);
    sess.user_id = Some(1);
    sess.character_id = Some(PILOT);
    let rep = ctx
        .dispatch_rpc(
            &mut sess,
            "charFittingMgr",
            None,
            "GetFittings",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("charFittingMgr.GetFittings");
    let PyRep::Rowset { columns, rows } = rep else {
        panic!("GetFittings must return a rowset, got {rep:?}")
    };
    assert_eq!(
        columns,
        ["fittingID", "name", "shipTypeID", "description", "items"]
    );
    rows.into_iter()
        .map(|r| {
            let items = match &r[4] {
                PyRep::List(v) => v
                    .iter()
                    .map(|t| match t {
                        PyRep::Tuple(x) => (
                            x[0].as_i64().unwrap(),
                            x[1].as_i64().unwrap(),
                            x[2].as_i64().unwrap(),
                        ),
                        other => panic!("fit item {other:?}"),
                    })
                    .collect(),
                other => panic!("items {other:?}"),
            };
            (
                r[0].as_i64().unwrap(),
                r[1].as_str().unwrap().to_string(),
                r[2].as_i64().unwrap(),
                items,
            )
        })
        .collect()
}

fn fittings_updated_count(ctx: &ServerContext) -> usize {
    ctx.drain_notifications_for_char(PILOT)
        .iter()
        .filter(|n| n.event == "OnFittingsUpdated" && n.args == vec![PyRep::Integer(PILOT)])
        .count()
}

#[tokio::test]
async fn test_esi_fittings_post_and_delete_scopes_errors_and_client_state() {
    // parity: esi:POST /characters/{character_id}/fittings, esi:DELETE /characters/{character_id}/fittings/{fitting_id}
    eve_testkit::attest(&[
        "esi:POST /characters/{character_id}/fittings",
        "esi:DELETE /characters/{character_id}/fittings/{fitting_id}",
    ]);
    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let now = ctx.now_unix();
    const SCOPE: &str = "esi-fittings.write_fittings.v1";
    for (method, raw) in [
        ("POST", "/v2/characters/90000001/fittings/"),
        ("DELETE", "/v1/characters/90000001/fittings/1/"),
    ] {
        let op = spec
            .match_operation(method, raw)
            .expect("pinned op")
            .operation;
        assert_eq!(
            op.required_scopes,
            vec![SCOPE.to_string()],
            "{method} scope"
        );
    }

    // Real SDE values: Rifter hull (category 6) and an SDE module (category 7).
    let rifter = &ctx.sde.types[&587];
    assert_eq!((rifter.name.as_str(), rifter.category_id), ("Rifter", 6));
    let module = &ctx.sde.types[&440];
    assert_eq!(module.category_id, 7, "type 440 is a module in the SDE");
    let unknown_type = ctx.sde.types.keys().max().copied().unwrap() + 1;

    ctx.db
        .acquire_online_lock(1, 91_001, Some(PILOT), true, now)
        .expect("online lock");
    let write = token_with_scopes(&ctx, PILOT, &[SCOPE]);
    let read_only = token_with_scopes(&ctx, PILOT, &["esi-fittings.read_fittings.v1"]);
    let other_char = token_with_scopes(&ctx, OTHER_PILOT, &[SCOPE]);
    let post_url = "/v2/characters/90000001/fittings/";
    let good_items = json!([
        { "type_id": 440, "flag": "MedSlot0", "quantity": 1 },
        { "type_id": 440, "flag": "Invalid", "quantity": 1 },
        { "type_id": 440, "flag": "ServiceSlot3", "quantity": 1 },
    ]);
    let post = |tok: Option<&str>, body: Vec<u8>| {
        let r = EsiHttpRequest::new("POST", post_url).with_body(body);
        match tok {
            Some(t) => r.with_bearer_token(t),
            None => r,
        }
    };

    // SSO scope enforcement + error model: 401 without token, 403 missing scope / wrong pilot.
    let r = send(
        &router,
        &post(None, fitting_body("A", 587, good_items.clone())),
        401,
    );
    assert_error(&r, 401, "Bearer token is required");
    let r = send(
        &router,
        &post(Some(&read_only), fitting_body("A", 587, good_items.clone())),
        403,
    );
    assert_error(&r, 403, SCOPE);
    let r = send(
        &router,
        &post(
            Some(&other_char),
            fitting_body("A", 587, good_items.clone()),
        ),
        403,
    );
    assert_error(&r, 403, "not authorized for character_id 90000001");

    // Request-schema validation (pinned constraints) -> 400 with the Error body.
    let long_name = "N".repeat(51);
    for (body, needle) in [
        (fitting_body("", 587, good_items.clone()), "1..=50"),
        (fitting_body(&long_name, 587, good_items.clone()), "1..=50"),
        (fitting_body("A", 587, json!([])), "1..=512"),
        (
            fitting_body("A", unknown_type, good_items.clone()),
            "Unknown ship_type_id",
        ),
        (
            fitting_body(
                "A",
                587,
                json!([{ "type_id": unknown_type, "flag": "Cargo", "quantity": 1 }]),
            ),
            "Unknown item type_id",
        ),
        (
            fitting_body(
                "A",
                587,
                json!([{ "type_id": 440, "flag": "BogusSlot", "quantity": 1 }]),
            ),
            "Invalid fitting item flag 'BogusSlot'",
        ),
        (
            fitting_body(
                "A",
                587,
                json!([{ "type_id": 440, "flag": "Invalid", "quantity": 1 }]),
            ),
            "no items left",
        ),
        (b"{not json".to_vec(), "Invalid fitting JSON body"),
    ] {
        let r = send(&router, &post(Some(&write), body), 400);
        assert_error(&r, 400, needle);
    }
    assert!(
        client_fittings(&ctx).await.is_empty(),
        "rejected POSTs must not persist"
    );
    assert_eq!(fittings_updated_count(&ctx), 0);

    // 201 Created: pinned body/header schema, private no-store caching, Invalid entry discarded.
    let created = send(
        &router,
        &post(Some(&write), fitting_body("ESI Rifter", 587, good_items)),
        201,
    );
    assert_eq!(created.header("Cache-Control"), Some("private, max-age=0"));
    assert!(created.header("ETag").is_some_and(|e| e.starts_with('"')));
    assert_eq!(created.header("Expires"), created.header("Last-Modified"));
    let fit_id = created.json().unwrap()["fitting_id"]
        .as_i64()
        .expect("fitting_id");
    assert_eq!(
        fittings_updated_count(&ctx),
        1,
        "ESI write notifies the logged-in client"
    );

    // Same game state the client sees: charFittingMgr.GetFittings returns the ESI fitting with
    // V24 invFlag numbers (MedSlot0 = 19, ServiceSlot3 = 167) and no 'Invalid' entry.
    let fits = client_fittings(&ctx).await;
    assert_eq!(fits.len(), 1);
    assert_eq!(fits[0].0, fit_id);
    assert_eq!((fits[0].1.as_str(), fits[0].2), ("ESI Rifter", 587));
    assert_eq!(
        fits[0].3,
        vec![
            (440, InventoryFlag::MedSlot0 as i64, 1),
            (440, InventoryFlag::ServiceSlot3 as i64, 1)
        ]
    );
    assert_eq!(
        (
            InventoryFlag::MedSlot0 as i64,
            InventoryFlag::ServiceSlot3 as i64
        ),
        (19, 167)
    );

    // A fitting owned by another pilot in WorldRepository.
    let foreign_fit = 9_100_777;
    ctx.db.with_mut_snapshot(|snap| {
        snap.saved_fittings.insert(
            foreign_fit,
            SavedFittingRecord {
                fitting_id: foreign_fit,
                owner_id: OTHER_PILOT,
                name: "Not yours".into(),
                ship_type_id: 587,
                description: String::new(),
                items: vec![(440, InventoryFlag::MedSlot0, 1)],
            },
        );
    });
    let del = |tok: Option<&str>, fid: i64| {
        let r = EsiHttpRequest::new("DELETE", format!("/v1/characters/90000001/fittings/{fid}/"));
        match tok {
            Some(t) => r.with_bearer_token(t),
            None => r,
        }
    };
    assert_error(
        &send(&router, &del(None, fit_id), 401),
        401,
        "Bearer token is required",
    );
    assert_error(
        &send(&router, &del(Some(&read_only), fit_id), 403),
        403,
        SCOPE,
    );
    assert_error(
        &send(&router, &del(Some(&write), foreign_fit), 403),
        403,
        "is not owned by character 90000001",
    );
    assert!(ctx
        .db
        .with_snapshot(|s| s.saved_fittings.contains_key(&foreign_fit)));

    // 204 No Content (pinned: empty body, Cache-Control/ETag/Last-Modified) and removal is
    // visible to the client RPC; a second delete is a 404 Error.
    let gone = send(&router, &del(Some(&write), fit_id), 204);
    assert!(gone.body.is_empty());
    assert_eq!(gone.header("Cache-Control"), Some("private, max-age=0"));
    assert_eq!(fittings_updated_count(&ctx), 1);
    assert!(client_fittings(&ctx).await.is_empty());
    assert_error(
        &send(&router, &del(Some(&write), fit_id), 404),
        404,
        &format!("Fitting {fit_id} not found"),
    );
}

#[tokio::test]
async fn test_esi_war_killmails_pagination_caching_and_world_state() {
    // parity: esi:GET /wars/{war_id}/killmails
    eve_testkit::attest(&["esi:GET /wars/{war_id}/killmails"]);
    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let op = OpenApiSpec::pinned()
        .match_operation("GET", "/v1/wars/1/killmails/")
        .expect("pinned op")
        .operation;
    assert!(
        op.required_scopes.is_empty(),
        "war killmails is a public endpoint"
    );
    let (aggr, def, bystander) = (98_100_001_i64, 98_100_002_i64, 98_100_003_i64);
    let bystander_pilot = 90_000_003_i64;
    let rifter = 587u32;
    assert_eq!(ctx.sde.types[&rifter].name, "Rifter");
    let jita = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.name == "Jita")
        .expect("Jita in SDE")
        .solar_system_id;
    ctx.db.with_mut_snapshot(|snap| {
        for (pilot, corp) in [
            (PILOT, aggr),
            (OTHER_PILOT, def),
            (bystander_pilot, bystander),
        ] {
            if let Some(ch) = snap.characters.get_mut(&pilot) {
                ch.corporation_id = corp;
            }
            snap.corporations.insert(
                corp,
                CorporationRecord {
                    corporation_id: corp,
                    name: format!("War Corp {corp}"),
                    ticker: format!("W{}", corp % 1000),
                    ceo_id: pilot,
                    master_wallet: IskCents::from_isk(1_000),
                    members: HashSet::from([pilot]),
                    ..Default::default()
                },
            );
        }
        snap.wars.insert(
            7001,
            WarRecord {
                war_id: 7001,
                aggressor_corp_id: aggr,
                defender_corp_id: def,
                mutual: false,
                started_unix: 0,
                active: true,
            },
        );
        // 1,001 war kills in both directions (pages of 1,000) plus 2 unrelated kills.
        for i in 0..1_001_i64 {
            let (victim, attacker) = if i % 2 == 0 {
                (OTHER_PILOT, PILOT)
            } else {
                (PILOT, OTHER_PILOT)
            };
            snap.killmails.insert(
                5_000_000 + i,
                KillmailRecord {
                    kill_id: 5_000_000 + i,
                    victim_character_id: victim,
                    attacker_character_id: attacker,
                    solar_system_id: jita,
                    ship_type_id: rifter,
                    total_value_isk: 1_000_000.0,
                },
            );
        }
        for (kid, victim, attacker) in [
            (6_000_000, bystander_pilot, PILOT),
            (6_000_001, OTHER_PILOT, bystander_pilot),
        ] {
            snap.killmails.insert(
                kid,
                KillmailRecord {
                    kill_id: kid,
                    victim_character_id: victim,
                    attacker_character_id: attacker,
                    solar_system_id: jita,
                    ship_type_id: rifter,
                    total_value_isk: 1.0,
                },
            );
        }
    });

    // Page 1: 1,000 newest war kills, X-Pages = 2, public 3600 s cache window.
    let p1 = send(
        &router,
        &EsiHttpRequest::new("GET", "/v1/wars/7001/killmails/"),
        200,
    );
    assert_eq!(p1.header("X-Pages"), Some("2"));
    assert_eq!(p1.header("Cache-Control"), Some("public, max-age=3600"));
    let body1 = p1.json().unwrap();
    let rows1 = body1.as_array().unwrap();
    assert_eq!(rows1.len(), 1_000);
    assert_eq!(rows1[0]["killmail_id"], json!(5_001_000));
    assert_eq!(
        rows1[0]["killmail_hash"],
        json!(compute_killmail_hash(5_001_000, OTHER_PILOT, PILOT, rifter))
    );
    assert_eq!(rows1[999]["killmail_id"], json!(5_000_001));
    assert_eq!(
        rows1[999]["killmail_hash"],
        json!(compute_killmail_hash(5_000_001, PILOT, OTHER_PILOT, rifter))
    );
    let ids: HashSet<i64> = rows1
        .iter()
        .map(|r| r["killmail_id"].as_i64().unwrap())
        .collect();
    assert!(
        !ids.contains(&6_000_000) && !ids.contains(&6_000_001),
        "non-war kills excluded"
    );

    // Expires = Last-Modified + 3600 s (exact HTTP-date of the virtual clock).
    let now = ctx.now_unix();
    assert_eq!(
        p1.header("Last-Modified").unwrap(),
        eve_server::esi::format_http_date(now)
    );
    assert_eq!(
        p1.header("Expires").unwrap(),
        eve_server::esi::format_http_date(now + 3600)
    );

    // Page 2: the single oldest kill; page 3 is a 404 Error; malformed page is a 400 Error.
    let p2 = send(
        &router,
        &EsiHttpRequest::new("GET", "/v1/wars/7001/killmails/?page=2"),
        200,
    );
    assert_eq!(p2.header("X-Pages"), Some("2"));
    assert_eq!(
        p2.json().unwrap(),
        json!([{
            "killmail_id": 5_000_000,
            "killmail_hash": compute_killmail_hash(5_000_000, OTHER_PILOT, PILOT, rifter),
        }])
    );
    let r = send(
        &router,
        &EsiHttpRequest::new("GET", "/v1/wars/7001/killmails/?page=3"),
        404,
    );
    assert_error(&r, 404, "Requested page 3 exceeds total pages 2");
    let r = send(
        &router,
        &EsiHttpRequest::new("GET", "/v1/wars/7001/killmails/?page=0"),
        400,
    );
    assert_error(&r, 400, "Invalid page parameter '0'");
    let r = send(
        &router,
        &EsiHttpRequest::new("GET", "/v1/wars/424242/killmails/"),
        404,
    );
    assert_error(&r, 404, "War 424242 not found");

    // Conditional GET: matching ETag -> 304 with empty body; the ETag changes after a new war
    // kill lands in WorldRepository.
    let etag = p1.header("ETag").unwrap().to_string();
    let r304 = send(
        &router,
        &EsiHttpRequest::new("GET", "/v1/wars/7001/killmails/").with_header("If-None-Match", &etag),
        304,
    );
    assert!(r304.body.is_empty());
    assert_eq!(r304.header("ETag"), Some(etag.as_str()));
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(
            5_002_000,
            KillmailRecord {
                kill_id: 5_002_000,
                victim_character_id: PILOT,
                attacker_character_id: OTHER_PILOT,
                solar_system_id: jita,
                ship_type_id: rifter,
                total_value_isk: 5.0,
            },
        );
    });
    let fresh = send(
        &router,
        &EsiHttpRequest::new("GET", "/v1/wars/7001/killmails/").with_header("If-None-Match", &etag),
        200,
    );
    assert_ne!(fresh.header("ETag"), Some(etag.as_str()));
    assert_eq!(fresh.json().unwrap()[0]["killmail_id"], json!(5_002_000));
}
