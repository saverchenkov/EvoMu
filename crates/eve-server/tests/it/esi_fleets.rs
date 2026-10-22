//! `ESI-7` Fleets acceptance tests against live `WorldRepository` state, validated against
//! `OpenApiSpec::pinned()`.
//!
//! Covers all 14 state-backed Fleet endpoints (`GET /characters/{character_id}/fleet`,
//! `GET/PUT /fleets/{fleet_id}`, `GET/POST /fleets/{fleet_id}/members`,
//! `PUT/DELETE /fleets/{fleet_id}/members/{member_id}`, `GET/POST /fleets/{fleet_id}/wings`,
//! `PUT/DELETE /fleets/{fleet_id}/wings/{wing_id}`, `POST /fleets/{fleet_id}/wings/{wing_id}/squads`,
//! `PUT/DELETE /fleets/{fleet_id}/squads/{squad_id}`) and the 5 `feat:esi.fleets.*` parity rows.

#![forbid(unsafe_code)]

use eve_db::{
    FleetMemberRecord, FleetRecord, FleetRole, FleetSquadRecord, FleetWingRecord, InventoryFlag,
    InventoryItemRecord,
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
            "evomu-esi-fleets-test",
            "https://localhost/callback",
            scopes,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-esi-fleets-test", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

/// Issues the request, asserts the status, validates status/headers/body against the pinned
/// OpenAPI operation, checks the caching contract (`ETag`, `Cache-Control`, `Expires`, `Last-Modified`),
/// and for `200 OK` verifies that a conditional re-request with `If-None-Match` returns `304 Not Modified`.
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
    if expected != 405 {
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

#[tokio::test]
async fn test_esi_fleets_settings_members_invite_move_and_kick_roundtrip() {
    // parity: esi:GET /characters/{character_id}/fleet, esi:GET /fleets/{fleet_id}, esi:PUT /fleets/{fleet_id}
    // parity: esi:GET /fleets/{fleet_id}/members, esi:POST /fleets/{fleet_id}/members
    // parity: esi:PUT /fleets/{fleet_id}/members/{member_id}, esi:DELETE /fleets/{fleet_id}/members/{member_id}
    // parity: feat:esi.fleets.char-fleet, feat:esi.fleets.fleet, feat:esi.fleets.members, feat:esi.fleets.member-move-kick
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/fleet",
        "esi:GET /fleets/{fleet_id}",
        "esi:PUT /fleets/{fleet_id}",
        "esi:GET /fleets/{fleet_id}/members",
        "esi:POST /fleets/{fleet_id}/members",
        "esi:PUT /fleets/{fleet_id}/members/{member_id}",
        "esi:DELETE /fleets/{fleet_id}/members/{member_id}",
        "feat:esi.fleets.char-fleet",
        "feat:esi.fleets.fleet",
        "feat:esi.fleets.members",
        "feat:esi.fleets.member-move-kick",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));

    let boss_id = 90_000_001_i64;
    let wingman_id = 90_000_002_i64;
    let outsider_uid = ctx
        .db
        .provision_player_account("outsider", "outsider-secret");
    let outsider_id = ctx
        .db
        .create_character(outsider_uid, "Outsider Pilot", 1, 1, 11, 17, &ctx.sde)
        .expect("create outsider character")
        .character_id;
    let fleet_id = 7_001_i64;
    let boss_ship_item_id = 990_701_i64;
    let boss_join_unix = 1_700_000_100_u64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(boss) = snap.characters.get_mut(&boss_id) {
            boss.solar_system_id = 30_000_142; // Jita
            boss.station_id = Some(60_003_760);
            boss.active_ship_id = boss_ship_item_id;
        }
        snap.items.insert(
            boss_ship_item_id,
            InventoryItemRecord {
                item_id: boss_ship_item_id,
                type_id: 587, // Rifter
                owner_id: boss_id,
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
        if let Some(wingman) = snap.characters.get_mut(&wingman_id) {
            wingman.solar_system_id = 30_002_187; // Amarr
            wingman.station_id = None;
            wingman.active_ship_id = 0; // No active ship item -> Capsule (670)
        }

        let mut wing_details = HashMap::new();
        wing_details.insert(
            1,
            FleetWingRecord {
                wing_id: 1,
                name: "Wing 1".to_string(),
                commander_id: None,
                squad_ids: vec![1],
            },
        );
        let mut squad_details = HashMap::new();
        squad_details.insert(
            1,
            FleetSquadRecord {
                squad_id: 1,
                wing_id: 1,
                name: "Squad 1".to_string(),
                commander_id: None,
            },
        );
        let mut member_details = HashMap::new();
        member_details.insert(
            boss_id,
            FleetMemberRecord {
                character_id: boss_id,
                role: FleetRole::FleetCommander,
                wing_id: None,
                squad_id: None,
                takes_fleet_warp: true,
                job: 0,
                role_name: "Fleet Commander".to_string(),
                join_time_unix: boss_join_unix,
            },
        );
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id,
                members: vec![boss_id],
                wings: vec![1],
                squads: vec![1],
                free_move: false,
                is_registered: true,
                wing_details,
                squad_details,
                member_details,
                invites: HashSet::new(),
                motd: String::new(),
            },
        );
    });

    let all_fleet_scopes = vec![
        "esi-fleets.read_fleet.v1".to_string(),
        "esi-fleets.write_fleet.v1".to_string(),
    ];
    let boss_token = issue_token_with_scopes(&ctx, boss_id, all_fleet_scopes.clone());
    let boss_read_only = issue_token_with_scopes(
        &ctx,
        boss_id,
        vec!["esi-fleets.read_fleet.v1".to_string()],
    );
    let boss_no_scope = issue_token_with_scopes(&ctx, boss_id, vec![]);
    let wingman_token = issue_token_with_scopes(&ctx, wingman_id, all_fleet_scopes.clone());
    let outsider_token = issue_token_with_scopes(&ctx, outsider_id, all_fleet_scopes);

    // 1. GET /characters/{character_id}/fleet
    let boss_fleet = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/characters/{boss_id}/fleet/"))
            .with_bearer_token(&boss_token),
        200,
    );
    assert_eq!(
        boss_fleet,
        json!({
            "fleet_id": fleet_id,
            "fleet_boss_id": boss_id,
            "wing_id": -1,
            "squad_id": -1,
            "role": "fleet_commander",
        })
    );
    // Auth, scope, character ownership, and not-in-fleet checks
    call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/characters/{boss_id}/fleet/")),
        401,
    );
    call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/characters/{boss_id}/fleet/"))
            .with_bearer_token(&boss_no_scope),
        403,
    );
    call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/characters/{wingman_id}/fleet/"))
            .with_bearer_token(&boss_token),
        403,
    );
    call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/characters/{outsider_id}/fleet/"))
            .with_bearer_token(&outsider_token),
        404,
    );

    // 2. GET /fleets/{fleet_id} & PUT /fleets/{fleet_id}
    let settings_initial = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/fleets/{fleet_id}/"))
            .with_bearer_token(&boss_token),
        200,
    );
    assert_eq!(
        settings_initial,
        json!({
            "motd": "",
            "is_free_move": false,
            "is_registered": true,
            "is_voice_enabled": false,
        })
    );
    call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/fleets/{fleet_id}/")),
        401,
    );
    call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/fleets/{fleet_id}/"))
            .with_bearer_token(&boss_no_scope),
        403,
    );
    call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/fleets/{fleet_id}/"))
            .with_bearer_token(&outsider_token),
        403,
    );
    call(
        &router,
        EsiHttpRequest::new("GET", "/v1/fleets/999999/").with_bearer_token(&boss_token),
        404,
    );

    // Update fleet MOTD and is_free_move via PUT /fleets/{fleet_id}
    call(
        &router,
        EsiHttpRequest::new("PUT", format!("/v1/fleets/{fleet_id}/"))
            .with_bearer_token(&boss_token)
            .with_body(
                serde_json::to_vec(&json!({
                    "motd": "Forming in Jita 4-4 — anchor on FC",
                    "is_free_move": true,
                }))
                .unwrap(),
            ),
        204,
    );
    let settings_updated = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/fleets/{fleet_id}/"))
            .with_bearer_token(&boss_token),
        200,
    );
    assert_eq!(
        settings_updated,
        json!({
            "motd": "Forming in Jita 4-4 — anchor on FC",
            "is_free_move": true,
            "is_registered": true,
            "is_voice_enabled": false,
        })
    );
    // Negative PUT /fleets/{fleet_id} checks
    call(
        &router,
        EsiHttpRequest::new("PUT", format!("/v1/fleets/{fleet_id}/"))
            .with_bearer_token(&boss_read_only)
            .with_body(br#"{"motd":"x"}"#.to_vec()),
        403,
    );
    call(
        &router,
        EsiHttpRequest::new("PUT", format!("/v1/fleets/{fleet_id}/"))
            .with_bearer_token(&outsider_token)
            .with_body(br#"{"motd":"x"}"#.to_vec()),
        403,
    );
    call(
        &router,
        EsiHttpRequest::new("PUT", format!("/v1/fleets/{fleet_id}/"))
            .with_bearer_token(&boss_token)
            .with_body(b"{}".to_vec()),
        400,
    );
    call(
        &router,
        EsiHttpRequest::new("PUT", "/v1/fleets/999999/")
            .with_bearer_token(&boss_token)
            .with_body(br#"{"motd":"x"}"#.to_vec()),
        404,
    );

    // 3. POST /fleets/{fleet_id}/members & GET /fleets/{fleet_id}/members
    ctx.advance_virtual_clock(120);
    let invite_unix = ctx.now_unix();
    call(
        &router,
        EsiHttpRequest::new("POST", format!("/v1/fleets/{fleet_id}/members/"))
            .with_bearer_token(&boss_token)
            .with_body(
                serde_json::to_vec(&json!({
                    "character_id": wingman_id,
                    "role": "squad_member",
                    "wing_id": 1,
                    "squad_id": 1,
                }))
                .unwrap(),
            ),
        204,
    );

    // Advance virtual clock further to prove GET /fleets/{fleet_id}/members uses persisted join_time_unix
    ctx.advance_virtual_clock(300);
    let members = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/fleets/{fleet_id}/members/"))
            .with_bearer_token(&boss_token),
        200,
    );
    let members_arr = members.as_array().expect("members array");
    assert_eq!(members_arr.len(), 2);
    assert_eq!(
        members_arr[0],
        json!({
            "character_id": boss_id,
            "ship_type_id": 587,
            "wing_id": -1,
            "squad_id": -1,
            "role": "fleet_commander",
            "role_name": "Fleet Commander",
            "join_time": format_iso8601_utc(boss_join_unix),
            "takes_fleet_warp": true,
            "solar_system_id": 30_000_142,
            "station_id": 60_003_760,
        })
    );
    assert_eq!(
        members_arr[1],
        json!({
            "character_id": wingman_id,
            "ship_type_id": 670,
            "wing_id": 1,
            "squad_id": 1,
            "role": "squad_member",
            "role_name": "Squad Member",
            "join_time": format_iso8601_utc(invite_unix),
            "takes_fleet_warp": true,
            "solar_system_id": 30_002_187,
        })
    );

    // Wingman can now query their own fleet membership and read fleet members, but cannot mutate fleet settings
    let wingman_fleet = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/characters/{wingman_id}/fleet/"))
            .with_bearer_token(&wingman_token),
        200,
    );
    assert_eq!(
        wingman_fleet,
        json!({
            "fleet_id": fleet_id,
            "fleet_boss_id": boss_id,
            "wing_id": 1,
            "squad_id": 1,
            "role": "squad_member",
        })
    );
    call(
        &router,
        EsiHttpRequest::new("PUT", format!("/v1/fleets/{fleet_id}/"))
            .with_bearer_token(&wingman_token)
            .with_body(br#"{"motd":"mutiny"}"#.to_vec()),
        403,
    );

    // Negative POST /fleets/{fleet_id}/members checks (including R-ESI7-P1-03 wing/squad validation)
    call(
        &router,
        EsiHttpRequest::new("POST", format!("/v1/fleets/{fleet_id}/members/"))
            .with_bearer_token(&boss_token)
            .with_body(br#"{"character_id":90000002,"role":"invalid_role"}"#.to_vec()),
        400,
    );
    call(
        &router,
        EsiHttpRequest::new("POST", format!("/v1/fleets/{fleet_id}/members/"))
            .with_bearer_token(&boss_token)
            .with_body(br#"{"character_id":99999999,"role":"squad_member"}"#.to_vec()),
        404,
    );
    call(
        &router,
        EsiHttpRequest::new("POST", format!("/v1/fleets/{fleet_id}/members/"))
            .with_bearer_token(&boss_token)
            .with_body(
                serde_json::to_vec(&json!({
                    "character_id": wingman_id,
                    "role": "squad_member",
                    "wing_id": 99999,
                    "squad_id": 1,
                }))
                .unwrap(),
            ),
        404,
    );
    call(
        &router,
        EsiHttpRequest::new("POST", format!("/v1/fleets/{fleet_id}/members/"))
            .with_bearer_token(&boss_token)
            .with_body(
                serde_json::to_vec(&json!({
                    "character_id": wingman_id,
                    "role": "squad_member",
                    "wing_id": 1,
                    "squad_id": 99999,
                }))
                .unwrap(),
            ),
        404,
    );

    // 4. PUT /fleets/{fleet_id}/members/{member_id} & DELETE /fleets/{fleet_id}/members/{member_id}
    // R-ESI7-P1-03: moving to wing_commander without wing_id or squad_member without wing_id/squad_id -> 400; nonexistent wing/squad -> 404
    call(
        &router,
        EsiHttpRequest::new("PUT", format!("/v1/fleets/{fleet_id}/members/{wingman_id}/"))
            .with_bearer_token(&boss_token)
            .with_body(br#"{"role":"wing_commander"}"#.to_vec()),
        400,
    );
    call(
        &router,
        EsiHttpRequest::new("PUT", format!("/v1/fleets/{fleet_id}/members/{wingman_id}/"))
            .with_bearer_token(&boss_token)
            .with_body(br#"{"role":"squad_member","wing_id":1}"#.to_vec()),
        400,
    );
    call(
        &router,
        EsiHttpRequest::new("PUT", format!("/v1/fleets/{fleet_id}/members/{wingman_id}/"))
            .with_bearer_token(&boss_token)
            .with_body(br#"{"role":"wing_commander","wing_id":99999}"#.to_vec()),
        404,
    );

    call(
        &router,
        EsiHttpRequest::new(
            "PUT",
            format!("/v1/fleets/{fleet_id}/members/{wingman_id}/"),
        )
        .with_bearer_token(&boss_token)
        .with_body(
            serde_json::to_vec(&json!({
                "role": "wing_commander",
                "wing_id": 1,
            }))
            .unwrap(),
        ),
        204,
    );
    let members_after_move = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/fleets/{fleet_id}/members/"))
            .with_bearer_token(&boss_token),
        200,
    );
    let moved_wingman = &members_after_move.as_array().unwrap()[1];
    assert_eq!(moved_wingman["role"], json!("wing_commander"));
    assert_eq!(moved_wingman["role_name"], json!("Wing Commander"));
    assert_eq!(moved_wingman["wing_id"], json!(1));
    assert_eq!(moved_wingman["squad_id"], json!(-1));
    assert_eq!(
        moved_wingman["join_time"],
        json!(format_iso8601_utc(invite_unix))
    );

    // R-ESI7-P1-01 & R-ESI7-P1-02: Wing commander cannot mutate fleet settings, create/delete wings, or move/kick the fleet boss
    call(
        &router,
        EsiHttpRequest::new("PUT", format!("/v1/fleets/{fleet_id}/"))
            .with_bearer_token(&wingman_token)
            .with_body(br#"{"motd":"wc cannot set motd"}"#.to_vec()),
        403,
    );
    call(
        &router,
        EsiHttpRequest::new("POST", format!("/v1/fleets/{fleet_id}/wings/"))
            .with_bearer_token(&wingman_token)
            .with_body(b"{}".to_vec()),
        403,
    );
    call(
        &router,
        EsiHttpRequest::new("DELETE", format!("/v1/fleets/{fleet_id}/wings/1/"))
            .with_bearer_token(&wingman_token),
        403,
    );
    call(
        &router,
        EsiHttpRequest::new("PUT", format!("/v1/fleets/{fleet_id}/members/{boss_id}/"))
            .with_bearer_token(&wingman_token)
            .with_body(br#"{"role":"squad_member","wing_id":1,"squad_id":1}"#.to_vec()),
        403,
    );
    call(
        &router,
        EsiHttpRequest::new("DELETE", format!("/v1/fleets/{fleet_id}/members/{boss_id}/"))
            .with_bearer_token(&wingman_token),
        403,
    );
    // R-ESI7-P1-02: Even the boss cannot DELETE /fleets/{fleet_id}/members/{boss_id} without transferring leadership
    call(
        &router,
        EsiHttpRequest::new("DELETE", format!("/v1/fleets/{fleet_id}/members/{boss_id}/"))
            .with_bearer_token(&boss_token),
        403,
    );

    // Kick member via DELETE /fleets/{fleet_id}/members/{member_id}
    call(
        &router,
        EsiHttpRequest::new(
            "DELETE",
            format!("/v1/fleets/{fleet_id}/members/{wingman_id}/"),
        )
        .with_bearer_token(&boss_token),
        204,
    );
    let members_after_kick = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/fleets/{fleet_id}/members/"))
            .with_bearer_token(&boss_token),
        200,
    );
    assert_eq!(members_after_kick.as_array().unwrap().len(), 1);
    call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/characters/{wingman_id}/fleet/"))
            .with_bearer_token(&wingman_token),
        404,
    );
    // Kicking already-removed member returns 404
    call(
        &router,
        EsiHttpRequest::new(
            "DELETE",
            format!("/v1/fleets/{fleet_id}/members/{wingman_id}/"),
        )
        .with_bearer_token(&boss_token),
        404,
    );
}

#[tokio::test]
async fn test_esi_fleets_wings_and_squads_hierarchy_lifecycle() {
    // parity: esi:GET /fleets/{fleet_id}/wings, esi:POST /fleets/{fleet_id}/wings
    // parity: esi:PUT /fleets/{fleet_id}/wings/{wing_id}, esi:DELETE /fleets/{fleet_id}/wings/{wing_id}
    // parity: esi:POST /fleets/{fleet_id}/wings/{wing_id}/squads
    // parity: esi:PUT /fleets/{fleet_id}/squads/{squad_id}, esi:DELETE /fleets/{fleet_id}/squads/{squad_id}
    // parity: feat:esi.fleets.wings-squads
    eve_testkit::attest(&[
        "esi:GET /fleets/{fleet_id}/wings",
        "esi:POST /fleets/{fleet_id}/wings",
        "esi:PUT /fleets/{fleet_id}/wings/{wing_id}",
        "esi:DELETE /fleets/{fleet_id}/wings/{wing_id}",
        "esi:POST /fleets/{fleet_id}/wings/{wing_id}/squads",
        "esi:PUT /fleets/{fleet_id}/squads/{squad_id}",
        "esi:DELETE /fleets/{fleet_id}/squads/{squad_id}",
        "feat:esi.fleets.wings-squads",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));

    let boss_id = 90_000_001_i64;
    let outsider_id = 90_000_002_i64;
    let fleet_id = 7_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut wing_details = HashMap::new();
        wing_details.insert(
            1,
            FleetWingRecord {
                wing_id: 1,
                name: "Wing 1".to_string(),
                commander_id: None,
                squad_ids: vec![1],
            },
        );
        let mut squad_details = HashMap::new();
        squad_details.insert(
            1,
            FleetSquadRecord {
                squad_id: 1,
                wing_id: 1,
                name: "Squad 1".to_string(),
                commander_id: None,
            },
        );
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id,
                members: vec![boss_id],
                wings: vec![1],
                squads: vec![1],
                free_move: true,
                is_registered: false,
                wing_details,
                squad_details,
                member_details: HashMap::new(),
                invites: HashSet::new(),
                motd: String::new(),
            },
        );
    });

    let all_fleet_scopes = vec![
        "esi-fleets.read_fleet.v1".to_string(),
        "esi-fleets.write_fleet.v1".to_string(),
    ];
    let boss_token = issue_token_with_scopes(&ctx, boss_id, all_fleet_scopes.clone());
    let outsider_token = issue_token_with_scopes(&ctx, outsider_id, all_fleet_scopes);

    // 1. Initial GET /fleets/{fleet_id}/wings
    let wings_initial = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/fleets/{fleet_id}/wings/"))
            .with_bearer_token(&boss_token),
        200,
    );
    assert_eq!(
        wings_initial,
        json!([
            {
                "id": 1,
                "name": "Wing 1",
                "squads": [
                    {
                        "id": 1,
                        "name": "Squad 1",
                    }
                ]
            }
        ])
    );

    // 2. Create a new wing via POST /fleets/{fleet_id}/wings -> 201 Created
    let created_wing = call(
        &router,
        EsiHttpRequest::new("POST", format!("/v1/fleets/{fleet_id}/wings/"))
            .with_bearer_token(&boss_token),
        201,
    );
    let new_wing_id = created_wing["wing_id"].as_i64().expect("wing_id");
    assert!(new_wing_id >= 2001);

    // 3. Rename the wing via PUT /fleets/{fleet_id}/wings/{wing_id} -> 204 No Content
    call(
        &router,
        EsiHttpRequest::new(
            "PUT",
            format!("/v1/fleets/{fleet_id}/wings/{new_wing_id}/"),
        )
        .with_bearer_token(&boss_token)
        .with_body(br#"{"name":"Alpha Wing"}"#.to_vec()),
        204,
    );

    // 4. Create two squads under the new wing via POST /fleets/{fleet_id}/wings/{wing_id}/squads -> 201 Created
    let created_sq1 = call(
        &router,
        EsiHttpRequest::new(
            "POST",
            format!("/v1/fleets/{fleet_id}/wings/{new_wing_id}/squads/"),
        )
        .with_bearer_token(&boss_token),
        201,
    );
    let sq1_id = created_sq1["squad_id"].as_i64().expect("squad_id 1");
    assert!(sq1_id >= 3001);

    let created_sq2 = call(
        &router,
        EsiHttpRequest::new(
            "POST",
            format!("/v1/fleets/{fleet_id}/wings/{new_wing_id}/squads/"),
        )
        .with_bearer_token(&boss_token),
        201,
    );
    let sq2_id = created_sq2["squad_id"].as_i64().expect("squad_id 2");
    assert!(sq2_id > sq1_id);

    // R-ESI7-P1-03: Inviting or moving a member with a squad_id that belongs to new_wing_id while specifying wing_id=1 returns 400
    call(
        &router,
        EsiHttpRequest::new("POST", format!("/v1/fleets/{fleet_id}/members/"))
            .with_bearer_token(&boss_token)
            .with_body(
                serde_json::to_vec(&json!({
                    "character_id": 90_000_002,
                    "role": "squad_member",
                    "wing_id": 1,
                    "squad_id": sq1_id,
                }))
                .unwrap(),
            ),
        400,
    );

    // 5. Rename squad 1 via PUT /fleets/{fleet_id}/squads/{squad_id} -> 204 No Content
    call(
        &router,
        EsiHttpRequest::new("PUT", format!("/v1/fleets/{fleet_id}/squads/{sq1_id}/"))
            .with_bearer_token(&boss_token)
            .with_body(br#"{"name":"Logi Sq"}"#.to_vec()),
        204,
    );

    // Verify full wing + squad hierarchy via GET /fleets/{fleet_id}/wings
    let wings_mid = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/fleets/{fleet_id}/wings/"))
            .with_bearer_token(&boss_token),
        200,
    );
    let wings_arr = wings_mid.as_array().unwrap();
    assert_eq!(wings_arr.len(), 2);
    let alpha_wing = wings_arr
        .iter()
        .find(|w| w["id"] == json!(new_wing_id))
        .expect("Alpha Wing in wings list");
    assert_eq!(alpha_wing["name"], json!("Alpha Wing"));
    let alpha_squads = alpha_wing["squads"].as_array().unwrap();
    assert_eq!(alpha_squads.len(), 2);
    assert_eq!(
        alpha_squads[0],
        json!({ "id": sq1_id, "name": "Logi Sq" })
    );
    assert_eq!(alpha_squads[1]["id"], json!(sq2_id));

    // 6. Delete squad 1 via DELETE /fleets/{fleet_id}/squads/{squad_id} -> 204 No Content
    call(
        &router,
        EsiHttpRequest::new("DELETE", format!("/v1/fleets/{fleet_id}/squads/{sq1_id}/"))
            .with_bearer_token(&boss_token),
        204,
    );
    let wings_after_sq_del = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/fleets/{fleet_id}/wings/"))
            .with_bearer_token(&boss_token),
        200,
    );
    let alpha_after_sq_del = wings_after_sq_del
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] == json!(new_wing_id))
        .unwrap();
    assert_eq!(
        alpha_after_sq_del["squads"].as_array().unwrap().len(),
        1,
        "deleted squad must be removed from wing"
    );

    // 7. Delete the wing via DELETE /fleets/{fleet_id}/wings/{wing_id} -> 204 No Content
    call(
        &router,
        EsiHttpRequest::new(
            "DELETE",
            format!("/v1/fleets/{fleet_id}/wings/{new_wing_id}/"),
        )
        .with_bearer_token(&boss_token),
        204,
    );
    let wings_final = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/fleets/{fleet_id}/wings/"))
            .with_bearer_token(&boss_token),
        200,
    );
    assert_eq!(wings_final.as_array().unwrap().len(), 1);
    assert_eq!(wings_final[0]["id"], json!(1));

    // 8. Negative tests: auth, non-member 403, name length > 10 -> 400, nonexistent fleet/wing/squad -> 404
    call(
        &router,
        EsiHttpRequest::new("POST", format!("/v1/fleets/{fleet_id}/wings/"))
            .with_bearer_token(&outsider_token),
        403,
    );
    call(
        &router,
        EsiHttpRequest::new("POST", "/v1/fleets/100/wings/")
            .with_bearer_token(&boss_token)
            .with_body(b"{}".to_vec()),
        404,
    );
    call(
        &router,
        EsiHttpRequest::new("PUT", format!("/v1/fleets/{fleet_id}/wings/1/"))
            .with_bearer_token(&boss_token)
            .with_body(br#"{"name":"TooLongWingName123"}"#.to_vec()),
        400,
    );
    call(
        &router,
        EsiHttpRequest::new("PUT", format!("/v1/fleets/{fleet_id}/squads/1/"))
            .with_bearer_token(&boss_token)
            .with_body(br#"{"name":""}"#.to_vec()),
        400,
    );
    call(
        &router,
        EsiHttpRequest::new("DELETE", format!("/v1/fleets/{fleet_id}/wings/999999/"))
            .with_bearer_token(&boss_token),
        404,
    );
    call(
        &router,
        EsiHttpRequest::new("DELETE", format!("/v1/fleets/{fleet_id}/squads/999999/"))
            .with_bearer_token(&boss_token),
        404,
    );
}
