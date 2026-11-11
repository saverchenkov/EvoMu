//! Edge-case and full-branch unit tests for `esi::character_routes`.

#![forbid(unsafe_code)]

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;
use eve_db::snapshot_ext::CharacterLoginStats;
use eve_db::{
    InventoryFlag, InventoryItemRecord, JumpCloneRecord, OwnerNoteRecord, SkillQueueEntry,
    TrainedSkillRecord,
};

fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn body(res: &RouteExecutionResult) -> &Value {
    res.body.as_ref().expect("response body")
}

#[test]
fn asset_coordinates_names_and_build_owner_assets_cover_all_branches() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let structure_id = 1_055_000_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        // 1. Item inside an Upwell structure hangar (flag != None -> [0,0,0], loc_type == "other")
        snap.items.insert(
            780_001,
            InventoryItemRecord {
                item_id: 780_001,
                type_id: 34,
                owner_id: char_id,
                location_id: structure_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );
        // 2. Item with flag == None anchored on a structure (returns st.position)
        snap.items.insert(
            780_002,
            InventoryItemRecord {
                item_id: 780_002,
                type_id: 34,
                owner_id: char_id,
                location_id: structure_id,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // 3. Item inside solar system without position -> fallback [0,0,0], loc_type == "solar_system"
        snap.items.insert(
            780_003,
            InventoryItemRecord {
                item_id: 780_003,
                type_id: 999_999_999, // unknown type_id -> fallback "Type 999999999"
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // 4. Parent in solar system with item_positions on root (not child) -> child inherits root pos
        snap.items.insert(
            780_004,
            InventoryItemRecord {
                item_id: 780_004,
                type_id: 3296,
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            780_005,
            InventoryItemRecord {
                item_id: 780_005,
                type_id: 34,
                owner_id: char_id,
                location_id: 780_004,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        snap.item_positions.insert(780_004, [111.0, 222.0, 333.0]);
        // Owner note fallback for 780_002 name
        snap.owner_notes.insert(
            -900_001,
            OwnerNoteRecord {
                note_id: -900_001,
                character_id: char_id,
                item_id: 780_002,
                label: "Note-Named-Item".into(),
                note: String::new(),
                timestamp_unix: 1_700_000_000,
            },
        );
        // Corp asset item
        snap.items.insert(
            780_006,
            InventoryItemRecord {
                item_id: 780_006,
                type_id: 34,
                owner_id: corp_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                singleton: false,
                ..Default::default()
            },
        );
    });

    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            resolve_item_coordinates(&ctx, snap, &snap.items[&780_001]),
            [0.0, 0.0, 0.0]
        );
        assert_eq!(
            resolve_item_coordinates(&ctx, snap, &snap.items[&780_002]),
            [10_000.0, 0.0, 0.0]
        );
        assert_eq!(
            resolve_item_coordinates(&ctx, snap, &snap.items[&780_003]),
            [0.0, 0.0, 0.0]
        );
        assert_eq!(
            resolve_item_coordinates(&ctx, snap, &snap.items[&780_005]),
            [111.0, 222.0, 333.0]
        );

        let assets = build_owner_assets(snap, char_id, 1);
        assert_eq!(assets.status_code, 200);
        let arr = body(&assets).as_array().unwrap();
        assert!(arr
            .iter()
            .any(|r| r["item_id"] == 780_001 && r["location_type"] == "other"));
        assert!(arr
            .iter()
            .any(|r| r["item_id"] == 780_003 && r["location_type"] == "solar_system"));
        assert_eq!(build_owner_assets(snap, char_id, 999).status_code, 404);

        assert_eq!(
            build_owner_asset_locations(&ctx, snap, char_id, b"bad").status_code,
            400
        );
        assert_eq!(
            build_owner_asset_names(&ctx, snap, char_id, b"bad").status_code,
            400
        );
        let names = build_owner_asset_names(&ctx, snap, char_id, br"[780002, 780003]");
        assert_eq!(names.status_code, 200);
        assert_eq!(body(&names)[0]["name"], "Note-Named-Item");
        assert_eq!(body(&names)[1]["name"], "Type 999999999");
    });

    // Dispatch routes for char & corp assets / locations / names (400, 404, 200)
    let p_bad = params(&[("character_id", "bad")]);
    let p_miss = params(&[("character_id", "99999999")]);
    let p_char = params(&[("character_id", &char_id.to_string())]);
    let p_bad_c = params(&[("corporation_id", "bad")]);
    let p_miss_c = params(&[("corporation_id", "99999999")]);
    let p_corp = params(&[("corporation_id", &corp_id.to_string())]);
    let empty_q = HashMap::new();

    for (method, path) in [
        ("GET", "/characters/{character_id}/assets"),
        ("POST", "/characters/{character_id}/assets/locations"),
        ("POST", "/characters/{character_id}/assets/names"),
    ] {
        assert_eq!(
            dispatch_character_routes(&ctx, method, path, &p_bad, &empty_q, None, 1, b"[]")
                .unwrap()
                .status_code,
            400
        );
        assert_eq!(
            dispatch_character_routes(&ctx, method, path, &p_miss, &empty_q, None, 1, b"[]")
                .unwrap()
                .status_code,
            404
        );
    }
    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "POST",
            "/characters/{character_id}/assets/locations",
            &p_char,
            &empty_q,
            None,
            1,
            br"[780001]"
        )
        .unwrap()
        .status_code,
        200
    );
    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "POST",
            "/characters/{character_id}/assets/names",
            &p_char,
            &empty_q,
            None,
            1,
            br"[780001]"
        )
        .unwrap()
        .status_code,
        200
    );

    for (method, path) in [
        ("GET", "/corporations/{corporation_id}/assets"),
        ("POST", "/corporations/{corporation_id}/assets/locations"),
        ("POST", "/corporations/{corporation_id}/assets/names"),
    ] {
        assert_eq!(
            dispatch_character_routes(&ctx, method, path, &p_bad_c, &empty_q, None, 1, b"[]")
                .unwrap()
                .status_code,
            400
        );
        assert_eq!(
            dispatch_character_routes(&ctx, method, path, &p_miss_c, &empty_q, None, 1, b"[]")
                .unwrap()
                .status_code,
            404
        );
    }
    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "POST",
            "/corporations/{corporation_id}/assets/locations",
            &p_corp,
            &empty_q,
            None,
            1,
            br"[780006]"
        )
        .unwrap()
        .status_code,
        200
    );
    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "POST",
            "/corporations/{corporation_id}/assets/names",
            &p_corp,
            &empty_q,
            None,
            1,
            br"[780006]"
        )
        .unwrap()
        .status_code,
        200
    );
}

#[test]
fn affiliation_portrait_standings_fatigue_blueprints_clones_and_implants_edge_cases() {
    let (ctx, _token, char_id, _corp_id, _planet_id) = seed_full_esi_context();
    let empty_q = HashMap::new();
    let p_bad = params(&[("character_id", "bad")]);
    let p_miss = params(&[("character_id", "99999999")]);
    let p_char = params(&[("character_id", &char_id.to_string())]);
    let now = ctx.now_unix();

    // POST /characters/affiliation bad JSON
    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "POST",
            "/characters/affiliation",
            &empty_q,
            &empty_q,
            None,
            1,
            b"not-json"
        )
        .unwrap()
        .status_code,
        400
    );

    // 400 and 404 across GET character routes
    for path in [
        "/characters/{character_id}/portrait",
        "/characters/{character_id}/standings",
        "/characters/{character_id}/fatigue",
        "/characters/{character_id}/blueprints",
        "/characters/{character_id}/clones",
        "/characters/{character_id}/implants",
        "/characters/{character_id}/skills",
        "/characters/{character_id}/attributes",
        "/characters/{character_id}/skillqueue",
        "/characters/{character_id}/location",
        "/characters/{character_id}/online",
        "/characters/{character_id}/ship",
        "/characters/{character_id}/fittings",
    ] {
        assert_eq!(
            dispatch_character_routes(&ctx, "GET", path, &p_bad, &empty_q, None, 1, b"")
                .unwrap()
                .status_code,
            400,
            "expected 400 for {path}"
        );
        assert_eq!(
            dispatch_character_routes(&ctx, "GET", path, &p_miss, &empty_q, None, 1, b"")
                .unwrap()
                .status_code,
            404,
            "expected 404 for {path}"
        );
    }

    // Seed rich standings, fatigue, blueprints, clones, implants, alpha skills, paused/active skillqueue, location, online, ship
    let agent_id = ctx.sde.agents.keys().next().copied().unwrap_or(3_008_416);
    let bp_tid = ctx
        .sde
        .blueprints
        .keys()
        .next()
        .copied()
        .unwrap_or(688);
    let skill_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 16)
        .map(|t| t.type_id)
        .unwrap_or(3402);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_001);
            ch.standings.insert(500_001, 2.5); // faction
            ch.standings.insert(1_000_035, 1.5); // npc_corp
            ch.standings.insert(agent_id, 3.0); // agent
            ch.standings.insert(999_888_777, 1.0); // unknown -> skipped
            ch.jump_fatigue_until_unix = 0;
            ch.jump_fatigue_minutes = 10.0;
            ch.home_station_id = 1_055_000_001; // structure home
            ch.last_clone_jump_unix = 1_700_000_000;
            ch.clone_grade = 1; // Alpha clone
            ch.neural.last_remap_unix = Some(1_700_000_000);
            ch.neural.remap_cooldown_until_unix = Some(1_730_000_000);
            ch.station_id = Some(1_055_000_001); // >= 100_000_000 -> structure_id
            ch.structure_id = None;
        }
        snap.character_last_jump_unix.insert(char_id, now);
        snap.jump_clones.insert(
            char_id,
            vec![
                JumpCloneRecord {
                    jump_clone_id: 1,
                    character_id: char_id,
                    location_id: 60_003_760,
                    implants: vec![9899],
                },
                JumpCloneRecord {
                    jump_clone_id: 2,
                    character_id: char_id,
                    location_id: 1_055_000_001,
                    implants: vec![],
                },
            ],
        );
        // Blueprints: BPC, singleton BPO, stacked BPO
        snap.items.insert(
            781_001,
            InventoryItemRecord {
                item_id: 781_001,
                type_id: bp_tid,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                is_blueprint_copy: true,
                blueprint_runs_remaining: Some(5),
                dynamic_attributes: HashMap::from([(1112, 10.0), (1113, 20.0)]),
                ..Default::default()
            },
        );
        snap.items.insert(
            781_002,
            InventoryItemRecord {
                item_id: 781_002,
                type_id: bp_tid,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                is_blueprint_copy: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            781_003,
            InventoryItemRecord {
                item_id: 781_003,
                type_id: bp_tid,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 3,
                singleton: false,
                is_blueprint_copy: false,
                ..Default::default()
            },
        );
        // Implant item on character
        snap.items.insert(
            781_004,
            InventoryItemRecord {
                item_id: 781_004,
                type_id: 9899,
                owner_id: char_id,
                location_id: char_id,
                flag: InventoryFlag::Implant,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.skills.entry(char_id).or_default().insert(
            skill_tid,
            TrainedSkillRecord {
                type_id: skill_tid,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.skill_queues.insert(
            char_id,
            vec![SkillQueueEntry {
                type_id: skill_tid,
                target_level: 5,
                start_sp: 45_255.0,
                end_sp: 0.0,
                duration_seconds: 0.0,
                started_at_unix: Some(now),
            }],
        );
        snap.character_login_stats.insert(
            char_id,
            CharacterLoginStats {
                last_login_unix: 0,
                last_logout_unix: 1_700_000_500,
                logins_count: 0,
            },
        );
    });

    // Exercise affiliation (war_faction_id + alliance_id), portrait (no doll info too)
    let aff = dispatch_character_routes(
        &ctx,
        "POST",
        "/characters/affiliation",
        &empty_q,
        &empty_q,
        None,
        1,
        &serde_json::to_vec(&vec![char_id]).unwrap(),
    )
    .unwrap();
    assert_eq!(body(&aff)[0]["faction_id"], 500_001);
    assert_eq!(body(&aff)[0]["alliance_id"], 99_000_001);

    let st_res = dispatch_character_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/standings",
        &p_char,
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert_eq!(body(&st_res).as_array().unwrap().len(), 3);

    let fat_res = dispatch_character_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/fatigue",
        &p_char,
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert!(body(&fat_res).get("jump_fatigue_expire_date").is_some());

    let bp_res = dispatch_character_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/blueprints",
        &p_char,
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert!(body(&bp_res).as_array().unwrap().len() >= 3);
    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "GET",
            "/characters/{character_id}/blueprints",
            &p_char,
            &empty_q,
            None,
            999,
            b""
        )
        .unwrap()
        .status_code,
        404
    );

    let clones = dispatch_character_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/clones",
        &p_char,
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert_eq!(body(&clones)["home_location"]["location_type"], "structure");
    assert!(body(&clones).get("last_clone_jump_date").is_some());

    let imps = dispatch_character_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/implants",
        &p_char,
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert!(body(&imps).as_array().unwrap().contains(&json!(9899)));

    let sk_alpha = dispatch_character_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/skills",
        &p_char,
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert_eq!(sk_alpha.status_code, 200);

    let attrs = dispatch_character_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/attributes",
        &p_char,
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert!(body(&attrs).get("last_remap_date").is_some());
    assert!(body(&attrs).get("accrued_remap_cooldown_date").is_some());

    // Active skillqueue with computed duration
    let sq_active = dispatch_character_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/skillqueue",
        &p_char,
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert!(body(&sq_active)[0].get("start_date").is_some());

    // Paused skillqueue omits start_date/finish_date
    ctx.db.with_mut_snapshot(|snap| {
        snap.paused_skill_queues.insert(char_id);
        snap.character_last_jump_unix.remove(&char_id);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.jump_fatigue_until_unix = now + 600;
            ch.structure_id = Some(1_055_000_001);
        }
    });
    let sq_paused = dispatch_character_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/skillqueue",
        &p_char,
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert!(body(&sq_paused)[0].get("start_date").is_none());

    let fat2 = dispatch_character_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/fatigue",
        &p_char,
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert!(body(&fat2).get("last_jump_date").is_none());
    assert!(body(&fat2).get("jump_fatigue_expire_date").is_some());

    // Location with explicit structure_id vs station_id >= 100M vs online session
    let loc1 = dispatch_character_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/location",
        &p_char,
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert_eq!(loc1.status_code, 200);
    assert!(body(&loc1).get("structure_id").is_some());

    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(uid, 55, Some(char_id), false, now);
    let online_res = dispatch_character_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/online",
        &p_char,
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert_eq!(body(&online_res)["online"], true);
    assert_eq!(body(&online_res)["logins"], 1);
    assert!(body(&online_res).get("last_logout").is_some());

    // Ship not found when active_ship_id missing
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = 999_888_777;
            ch.structure_id = None;
            ch.station_id = Some(1_055_000_001);
        }
    });
    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "GET",
            "/characters/{character_id}/ship",
            &p_char,
            &empty_q,
            None,
            1,
            b""
        )
        .unwrap()
        .status_code,
        404
    );
    let loc2 = dispatch_character_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/location",
        &p_char,
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert_eq!(body(&loc2)["structure_id"], 1_055_000_001_i64);
}

#[test]
fn fittings_post_and_delete_cover_all_validation_errors_and_ownership_checks() {
    let (ctx, _token, char_id, _corp_id, _planet_id) = seed_full_esi_context();
    let empty_q = HashMap::new();
    let p_bad = params(&[("character_id", "bad")]);
    let p_miss = params(&[("character_id", "99999999")]);
    let p_char = params(&[("character_id", &char_id.to_string())]);

    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "POST",
            "/characters/{character_id}/fittings",
            &p_bad,
            &empty_q,
            None,
            1,
            b"{}"
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "POST",
            "/characters/{character_id}/fittings",
            &p_char,
            &empty_q,
            None,
            1,
            b"not-json"
        )
        .unwrap()
        .status_code,
        400
    );

    let long_name = "a".repeat(51);
    let long_desc = "d".repeat(501);
    let too_many_items: Vec<Value> = (0..513)
        .map(|_| json!({"type_id": 439, "flag": "Cargo", "quantity": 1}))
        .collect();

    for bad_fit in [
        json!({"description": "d", "ship_type_id": 587, "items": [{"type_id": 439, "flag": "Cargo", "quantity": 1}]}),
        json!({"name": long_name, "description": "d", "ship_type_id": 587, "items": [{"type_id": 439, "flag": "Cargo", "quantity": 1}]}),
        json!({"name": "Ok", "ship_type_id": 587, "items": [{"type_id": 439, "flag": "Cargo", "quantity": 1}]}),
        json!({"name": "Ok", "description": long_desc, "ship_type_id": 587, "items": [{"type_id": 439, "flag": "Cargo", "quantity": 1}]}),
        json!({"name": "Ok", "description": "d", "items": [{"type_id": 439, "flag": "Cargo", "quantity": 1}]}),
        json!({"name": "Ok", "description": "d", "ship_type_id": -1, "items": [{"type_id": 439, "flag": "Cargo", "quantity": 1}]}),
        json!({"name": "Ok", "description": "d", "ship_type_id": 587}),
        json!({"name": "Ok", "description": "d", "ship_type_id": 587, "items": too_many_items}),
        json!({"name": "Ok", "description": "d", "ship_type_id": 587, "items": [{"flag": "Cargo", "quantity": 1}]}),
        json!({"name": "Ok", "description": "d", "ship_type_id": 587, "items": [{"type_id": -1, "flag": "Cargo", "quantity": 1}]}),
        json!({"name": "Ok", "description": "d", "ship_type_id": 587, "items": [{"type_id": 99999999, "flag": "Cargo", "quantity": 1}]}),
        json!({"name": "Ok", "description": "d", "ship_type_id": 587, "items": [{"type_id": 439, "quantity": 1}]}),
        json!({"name": "Ok", "description": "d", "ship_type_id": 587, "items": [{"type_id": 439, "flag": "Cargo"}]}),
        json!({"name": "Ok", "description": "d", "ship_type_id": 587, "items": [{"type_id": 439, "flag": "Cargo", "quantity": 9_999_999_999_i64}]}),
        json!({"name": "Ok", "description": "d", "ship_type_id": 587, "items": [{"type_id": 439, "flag": "Cargo", "quantity": 0}]}),
        json!({"name": "Ok", "description": "d", "ship_type_id": 587, "items": [{"type_id": 439, "flag": "Invalid", "quantity": 1}]}),
    ] {
        assert_eq!(
            dispatch_character_routes(
                &ctx,
                "POST",
                "/characters/{character_id}/fittings",
                &p_char,
                &empty_q,
                None,
                1,
                &serde_json::to_vec(&bad_fit).unwrap()
            )
            .unwrap()
            .status_code,
            400
        );
    }

    let valid_fit = json!({
        "name": "Valid",
        "description": "Desc",
        "ship_type_id": 587,
        "items": [{"type_id": 439, "flag": "Cargo", "quantity": 1}]
    });
    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "POST",
            "/characters/{character_id}/fittings",
            &p_miss,
            &empty_q,
            None,
            1,
            &serde_json::to_vec(&valid_fit).unwrap()
        )
        .unwrap()
        .status_code,
        404
    );

    let created = dispatch_character_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/fittings",
        &p_char,
        &empty_q,
        None,
        1,
        &serde_json::to_vec(&valid_fit).unwrap(),
    )
    .unwrap();
    let fid = body(&created)["fitting_id"].as_i64().unwrap();

    // DELETE fitting errors: 400 char, 400 fit, 404 char, 404 fit, 403 non-owner
    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "DELETE",
            "/characters/{character_id}/fittings/{fitting_id}",
            &params(&[("character_id", "bad"), ("fitting_id", "1")]),
            &empty_q,
            None,
            1,
            b""
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "DELETE",
            "/characters/{character_id}/fittings/{fitting_id}",
            &params(&[("character_id", &char_id.to_string()), ("fitting_id", "bad")]),
            &empty_q,
            None,
            1,
            b""
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "DELETE",
            "/characters/{character_id}/fittings/{fitting_id}",
            &params(&[("character_id", "99999999"), ("fitting_id", &fid.to_string())]),
            &empty_q,
            None,
            1,
            b""
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "DELETE",
            "/characters/{character_id}/fittings/{fitting_id}",
            &params(&[("character_id", &char_id.to_string()), ("fitting_id", "99999999")]),
            &empty_q,
            None,
            1,
            b""
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "DELETE",
            "/characters/{character_id}/fittings/{fitting_id}",
            &params(&[("character_id", "90000002"), ("fitting_id", &fid.to_string())]),
            &empty_q,
            None,
            1,
            b""
        )
        .unwrap()
        .status_code,
        403
    );
}
