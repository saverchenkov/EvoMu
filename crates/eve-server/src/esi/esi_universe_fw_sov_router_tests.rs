//! Comprehensive unit and mutation-killing tests for ESI universe, sovereignty, faction warfare, OpenAPI, and HTTP router modules.

use super::full_endpoint_catalog_tests::seed_full_esi_context;
use super::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_db::{
    FwCharacterStatsRecord, FwSystemOccupationRecord, FwWindowedStat, SovCampaignScoreRecord,
    SystemJumpEvent, SystemKillEvent, UpwellStructureRecord,
};
use serde_json::{json, Value};
use std::sync::Arc;

fn call_json(
    router: &EsiRouter,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut req = EsiHttpRequest::new(method, path).with_bearer_token(token);
    if let Some(b) = body {
        req = req
            .with_header("Content-Type", "application/json")
            .with_body(serde_json::to_vec(&b).unwrap());
    }
    let resp = router.handle_request(&req);
    let val = if resp.body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&resp.body).unwrap_or(Value::Null)
    };
    (resp.status_code, val)
}

#[test]
fn universe_names_ids_structures_and_autopilot_waypoint_branches() {
    let (ctx, token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let router = EsiRouter::new(Arc::clone(&ctx));
    let alliance_id = 99_000_001_i64;
    let struct_id = 1_055_000_001_i64;
    let offline_struct_id = 1_055_000_002_i64;
    let private_struct_id = 1_055_000_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            offline_struct_id,
            UpwellStructureRecord {
                structure_id: offline_struct_id,
                type_id: 35_832,
                owner_corp_id: 98_999_999,
                solar_system_id: 30_000_142,
                name: "Unfueled Structure".into(),
                position: [20_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: false,
                fuel_blocks: 0,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.structures.insert(
            private_struct_id,
            UpwellStructureRecord {
                structure_id: private_struct_id,
                type_id: 35_832,
                owner_corp_id: 98_999_999,
                solar_system_id: 30_000_142,
                name: "Secret Corp Citadel".into(),
                position: [50_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        let mut ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        for sid in [struct_id, offline_struct_id] {
            ext.access_profiles.insert(
                sid,
                crate::services::station_structure::structure_upwell_ops::StructureAccessProfile {
                    corp_only: false,
                    ..Default::default()
                },
            );
        }
        ext.access_profiles.insert(
            private_struct_id,
            crate::services::station_structure::structure_upwell_ops::StructureAccessProfile {
                corp_only: true,
                ..Default::default()
            },
        );
        crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
        snap.structure_docking_acl_denied
            .entry(offline_struct_id)
            .or_default()
            .insert(char_id);
    });

    // /universe/structures filter validation and active service filtering
    assert_eq!(
        call_json(
            &router,
            "GET",
            "/latest/universe/structures?filter=bad_filter",
            &token,
            None,
        )
        .0,
        400
    );
    let (_, all_structs) = call_json(&router, "GET", "/latest/universe/structures", &token, None);
    assert!(all_structs.as_array().unwrap().contains(&json!(struct_id)));
    assert!(all_structs
        .as_array()
        .unwrap()
        .contains(&json!(offline_struct_id)));
    assert!(!all_structs
        .as_array()
        .unwrap()
        .contains(&json!(private_struct_id)));

    let (_, market_structs) = call_json(
        &router,
        "GET",
        "/latest/universe/structures?filter=market",
        &token,
        None,
    );
    assert!(market_structs.as_array().unwrap().contains(&json!(struct_id)));
    assert!(!market_structs
        .as_array()
        .unwrap()
        .contains(&json!(offline_struct_id)));

    // /universe/structures/{structure_id}: 404, 403 (not on ACL for other corp structure), 200
    assert_eq!(
        call_json(
            &router,
            "GET",
            "/latest/universe/structures/999999999",
            &token,
            None,
        )
        .0,
        404
    );
    assert_eq!(
        call_json(
            &router,
            "GET",
            &format!("/latest/universe/structures/{offline_struct_id}"),
            &token,
            None,
        )
        .0,
        403
    );
    assert_eq!(
        call_json(
            &router,
            "GET",
            &format!("/latest/universe/structures/{struct_id}"),
            &token,
            None,
        )
        .0,
        200
    );

    // /universe/names validation (empty, duplicate, unknown ID) and multi-category resolution
    assert_eq!(
        call_json(&router, "POST", "/latest/universe/names", &token, Some(json!([]))).0,
        400
    );
    assert_eq!(
        call_json(
            &router,
            "POST",
            "/latest/universe/names",
            &token,
            Some(json!([34, 34])),
        )
        .0,
        400
    );
    assert_eq!(
        call_json(
            &router,
            "POST",
            "/latest/universe/names",
            &token,
            Some(json!([999_888_777])),
        )
        .0,
        404
    );
    let (st_names, names_val) = call_json(
        &router,
        "POST",
        "/latest/universe/names",
        &token,
        Some(json!([
            alliance_id,
            char_id,
            corp_id,
            1_000_125,
            500_001,
            10_000_002,
            20_000_020,
            30_000_142,
            34,
            60_003_760
        ])),
    );
    assert_eq!(st_names, 200);
    assert_eq!(names_val.as_array().unwrap().len(), 10);

    // /universe/ids validation (empty array, blank string, case-insensitive duplicate) and resolution
    assert_eq!(
        call_json(&router, "POST", "/latest/universe/ids", &token, Some(json!([]))).0,
        400
    );
    assert_eq!(
        call_json(
            &router,
            "POST",
            "/latest/universe/ids",
            &token,
            Some(json!(["   "])),
        )
        .0,
        400
    );
    assert_eq!(
        call_json(
            &router,
            "POST",
            "/latest/universe/ids",
            &token,
            Some(json!(["Jita", "jita"])),
        )
        .0,
        400
    );
    let (st_ids, ids_val) = call_json(
        &router,
        "POST",
        "/latest/universe/ids",
        &token,
        Some(json!([
            "Test Alliance",
            "Test Corporation",
            "Caldari State",
            "The Forge",
            "Kimotoro",
            "Jita",
            "Tritanium"
        ])),
    );
    assert_eq!(st_ids, 200);
    assert!(!ids_val["systems"].as_array().unwrap().is_empty());
    assert!(!ids_val["inventory_types"].as_array().unwrap().is_empty());

    // /ui/autopilot/waypoint parameter validation, 404, 204 online, and 501 offline
    assert_eq!(
        call_json(&router, "POST", "/latest/ui/autopilot/waypoint", &token, None).0,
        400
    );
    assert_eq!(
        call_json(
            &router,
            "POST",
            "/latest/ui/autopilot/waypoint?destination_id=30000142&add_to_beginning=bad&clear_other_waypoints=false",
            &token,
            None,
        )
        .0,
        400
    );
    assert_eq!(
        call_json(
            &router,
            "POST",
            "/latest/ui/autopilot/waypoint?destination_id=999999999&add_to_beginning=false&clear_other_waypoints=false",
            &token,
            None,
        )
        .0,
        404
    );
    // Offline returns 501, then acquire online lock and verify 204
    assert_eq!(
        call_json(
            &router,
            "POST",
            "/latest/ui/autopilot/waypoint?destination_id=30000142&add_to_beginning=false&clear_other_waypoints=true",
            &token,
            None,
        )
        .0,
        501
    );
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(uid, 999, Some(char_id), false, ctx.now_unix());
    assert_eq!(
        call_json(
            &router,
            "POST",
            "/latest/ui/autopilot/waypoint?destination_id=30000142&add_to_beginning=false&clear_other_waypoints=true",
            &token,
            None,
        )
        .0,
        204
    );
    assert_eq!(
        call_json(
            &router,
            "POST",
            "/latest/ui/autopilot/waypoint?destination_id=30000144&add_to_beginning=true&clear_other_waypoints=false",
            &token,
            None,
        )
        .0,
        204
    );
    let wps = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].autopilot_waypoints.clone());
    assert_eq!(wps, vec![30_000_144, 30_000_142]);

    ctx.db.release_online_lock(uid, 999);
    assert_eq!(
        call_json(
            &router,
            "POST",
            "/latest/ui/autopilot/waypoint?destination_id=30000142&add_to_beginning=false&clear_other_waypoints=false",
            &token,
            None,
        )
        .0,
        501
    );
}

#[test]
fn sov_fw_system_jumps_kills_openapi_and_router_protocol_edges() {
    let (ctx, token, char_id, _corp_id, _planet_id) = seed_full_esi_context();
    let router = EsiRouter::new(Arc::clone(&ctx));
    let now = ctx.now_unix();

    // OpenApiSpec parser error branches
    assert!(OpenApiSpec::from_json_str("not_json").is_err());
    assert!(OpenApiSpec::from_json_str("{}").is_err());

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_character_stats.insert(
            char_id,
            FwCharacterStatsRecord {
                character_id: char_id,
                faction_id: Some(500_001),
                enlisted_on_unix: Some(now.saturating_sub(86_400)),
                current_rank: 3,
                highest_rank: 4,
                kills: FwWindowedStat {
                    yesterday: 5,
                    last_week: 15,
                    total: 40,
                },
                victory_points: FwWindowedStat {
                    yesterday: 200,
                    last_week: 800,
                    total: 3_000,
                },
            },
        );
        snap.fw_system_occupations.insert(
            30_000_142,
            FwSystemOccupationRecord {
                solar_system_id: 30_000_142,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_004,
                victory_points: 1_500,
                victory_points_threshold: 3_000,
            },
        );
        snap.fw_system_occupations.insert(
            30_000_144,
            FwSystemOccupationRecord {
                solar_system_id: 30_000_144,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 3_000,
                victory_points_threshold: 3_000,
            },
        );
        snap.fw_wars = vec![(500_001, 500_004)];
        snap.sov_campaign_scores.insert(
            1_055_000_001,
            SovCampaignScoreRecord {
                structure_id: 1_055_000_001,
                attackers_score: 0.4,
                defender_score: 0.6,
            },
        );
        snap.system_jump_events.push(SystemJumpEvent {
            solar_system_id: 30_000_142,
            character_id: char_id,
            timestamp_unix: now.saturating_sub(60),
        });
        snap.system_kill_events.push(SystemKillEvent {
            solar_system_id: 30_000_142,
            ship_kills: 3,
            npc_kills: 12,
            pod_kills: 1,
            timestamp_unix: now.saturating_sub(60),
        });
    });

    for path in [
        "/latest/fw/leaderboards",
        "/latest/fw/leaderboards/characters",
        "/latest/fw/leaderboards/corporations",
        "/latest/fw/stats",
        "/latest/fw/systems",
        "/latest/fw/wars",
        "/latest/sovereignty/map",
        "/latest/sovereignty/structures",
        "/latest/sovereignty/campaigns",
        "/latest/universe/system_jumps",
        "/latest/universe/system_kills",
        "/latest/opportunities/groups",
        "/latest/opportunities/tasks",
        &format!("/latest/characters/{char_id}/opportunities"),
    ] {
        let (st, _) = call_json(&router, "GET", path, &token, None);
        assert_eq!(st, 200, "GET {path}");
    }

    // Router protocol edges: /meta, /versions, /swagger.json, /status.json, ETag 304, 405, 401, 403
    let meta_resp = router.handle_request(&EsiHttpRequest::new("GET", "/meta"));
    assert_eq!(meta_resp.status_code, 200);

    let stat_json_resp = router.handle_request(&EsiHttpRequest::new("GET", "/status.json"));
    assert_eq!(stat_json_resp.status_code, 200);

    let ver_resp = router.handle_request(&EsiHttpRequest::new("GET", "/versions"));
    assert_eq!(ver_resp.status_code, 200);

    let swag_resp = router.handle_request(&EsiHttpRequest::new("GET", "/swagger.json"));
    assert_eq!(swag_resp.status_code, 200);

    // ETag conditional GET -> 304 Not Modified
    let first_get = router.handle_request(&EsiHttpRequest::new("GET", "/latest/alliances"));
    assert_eq!(first_get.status_code, 200);
    let etag = first_get.headers.get("ETag").cloned().unwrap();
    let cond_get = router.handle_request(
        &EsiHttpRequest::new("GET", "/latest/alliances").with_header("If-None-Match", &etag),
    );
    assert_eq!(cond_get.status_code, 304);

    // 405 Method Not Allowed on existing path
    let m405 = router.handle_request(&EsiHttpRequest::new("DELETE", "/latest/status"));
    assert_eq!(m405.status_code, 405);

    // 401 missing token on authenticated route, and 403 wrong character_id
    let m401 = router.handle_request(&EsiHttpRequest::new(
        "GET",
        format!("/latest/characters/{char_id}/wallet"),
    ));
    assert_eq!(m401.status_code, 401);

    let m403 = router.handle_request(
        &EsiHttpRequest::new("GET", "/latest/characters/90000002/wallet")
            .with_bearer_token(&token),
    );
    assert_eq!(m403.status_code, 403);
}
