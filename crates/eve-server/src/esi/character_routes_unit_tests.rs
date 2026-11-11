//! Unit and mutation-killing tests for `esi::character_routes`.

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;

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
fn parse_unique_id_array_and_asset_helpers_cover_all_branches() {
    assert_eq!(
        parse_unique_id_array(b"not-json", 10)
            .expect_err("bad json")
            .status_code,
        400
    );
    assert_eq!(
        parse_unique_id_array(b"[]", 10)
            .expect_err("empty")
            .status_code,
        400
    );
    assert_eq!(
        parse_unique_id_array(b"[1, 2, 3, 4]", 3)
            .expect_err("exceeds max")
            .status_code,
        400
    );
    assert_eq!(
        parse_unique_id_array(b"[10, 20, 10]", 10)
            .expect_err("duplicate")
            .status_code,
        400
    );
    assert_eq!(
        parse_unique_id_array(b"[10, 20, 30]", 3).unwrap(),
        vec![10, 20, 30]
    );

    let (ctx, _token, char_id, _corp_id, _planet_id) = seed_full_esi_context();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            770_001,
            eve_db::InventoryItemRecord {
                item_id: 770_001,
                type_id: 3296,
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
            770_002,
            eve_db::InventoryItemRecord {
                item_id: 770_002,
                type_id: 34,
                owner_id: char_id,
                location_id: 770_001,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                is_blueprint_copy: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            770_003,
            eve_db::InventoryItemRecord {
                item_id: 770_003,
                type_id: 587,
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                is_blueprint_copy: false,
                ..Default::default()
            },
        );
        snap.item_positions
            .insert(770_003, [12_500.0, -4_000.0, 999.5]);
        snap.item_custom_names
            .insert(770_001, "My Station Box".to_string());
    });
    ctx.esi_state.set_custom_item_name(770_003, "Named Rifter");

    ctx.db.with_snapshot(|snap| {
        let box_item = snap.items.get(&770_001).unwrap();
        let nested_item = snap.items.get(&770_002).unwrap();
        let space_item = snap.items.get(&770_003).unwrap();
        assert_eq!(
            resolve_item_coordinates(&ctx, snap, box_item),
            [0.0, 0.0, 0.0]
        );
        assert_eq!(
            resolve_item_coordinates(&ctx, snap, nested_item),
            [0.0, 0.0, 0.0]
        );
        assert_eq!(
            resolve_item_coordinates(&ctx, snap, space_item),
            [12_500.0, -4_000.0, 999.5]
        );

        let assets_res = build_owner_assets(snap, char_id, 1);
        assert_eq!(assets_res.status_code, 200);
        let arr = body(&assets_res).as_array().unwrap();
        let bpc_row = arr
            .iter()
            .find(|r| r["item_id"] == 770_002)
            .expect("nested item in assets");
        assert_eq!(bpc_row["location_type"], "item");
        assert_eq!(bpc_row["is_blueprint_copy"], true);

        let locs_res =
            build_owner_asset_locations(&ctx, snap, char_id, br"[770001, 770002, 770003]");
        assert_eq!(locs_res.status_code, 200);
        let loc_arr = body(&locs_res).as_array().unwrap();
        assert_eq!(loc_arr[2]["position"]["x"], 12_500.0);

        assert_eq!(
            build_owner_asset_locations(&ctx, snap, char_id, br"[999999999]").status_code,
            404
        );
        assert_eq!(
            build_owner_asset_names(&ctx, snap, char_id, br"[999999999]").status_code,
            404
        );

        let names_res =
            build_owner_asset_names(&ctx, snap, char_id, br"[770001, 770002, 770003]");
        assert_eq!(names_res.status_code, 200);
        let n_arr = body(&names_res).as_array().unwrap();
        assert_eq!(n_arr[0]["name"], "My Station Box");
        assert_eq!(n_arr[2]["name"], "Named Rifter");
    });
}

#[test]
fn dispatch_affiliation_portrait_standings_fatigue_blueprints_clones_and_implants() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let char_str = char_id.to_string();
    let p_char = params(&[("character_id", &char_str)]);
    let empty_q = HashMap::new();

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
    assert_eq!(aff.status_code, 200);
    assert_eq!(body(&aff)[0]["character_id"], char_id);
    assert_eq!(body(&aff)[0]["corporation_id"], corp_id);

    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "POST",
            "/characters/affiliation",
            &empty_q,
            &empty_q,
            None,
            1,
            br"[99999999]"
        )
        .unwrap()
        .status_code,
        404
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.character_doll_info
            .insert(char_id, ("dna:123/456?a=b#c d".to_string(), String::new()));
    });
    let port = dispatch_character_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/portrait",
        &p_char,
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert_eq!(port.status_code, 200);
    assert!(body(&port)["px64x64"]
        .as_str()
        .unwrap()
        .contains("&dna=dna%3A123%2F456%3Fa%3Db%23c%20d"));

    for path in [
        "/characters/{character_id}/standings",
        "/characters/{character_id}/fatigue",
        "/characters/{character_id}/blueprints",
        "/characters/{character_id}/clones",
        "/characters/{character_id}/implants",
    ] {
        let res =
            dispatch_character_routes(&ctx, "GET", path, &p_char, &empty_q, None, 1, b"").unwrap();
        assert_eq!(res.status_code, 200, "failed on {path}");
    }
}

#[test]
fn dispatch_skills_attributes_skillqueue_location_fittings_and_assets() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let char_str = char_id.to_string();
    let corp_str = corp_id.to_string();
    let p_char = params(&[("character_id", &char_str)]);
    let p_corp = params(&[("corporation_id", &corp_str)]);
    let empty_q = HashMap::new();

    let skills_omega = dispatch_character_routes(
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
    assert_eq!(skills_omega.status_code, 200);
    assert!(body(&skills_omega)["total_sp"].as_i64().unwrap() > 0);

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
    assert_eq!(attrs.status_code, 200);
    assert!(body(&attrs)["intelligence"].as_i64().unwrap() >= 17);

    let sq = dispatch_character_routes(
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
    assert_eq!(sq.status_code, 200);
    assert!(body(&sq).is_array());

    for path in [
        "/characters/{character_id}/location",
        "/characters/{character_id}/online",
        "/characters/{character_id}/ship",
    ] {
        let res =
            dispatch_character_routes(&ctx, "GET", path, &p_char, &empty_q, None, 1, b"").unwrap();
        assert_eq!(res.status_code, 200, "failed on {path}");
    }

    for bad_fit in [
        json!({"name": "", "description": "d", "ship_type_id": 587, "items": [{"type_id": 439, "flag": "LoSlot0", "quantity": 1}]}),
        json!({"name": "Fit", "description": "d", "ship_type_id": 99999999, "items": [{"type_id": 439, "flag": "LoSlot0", "quantity": 1}]}),
        json!({"name": "Fit", "description": "d", "ship_type_id": 587, "items": []}),
        json!({"name": "Fit", "description": "d", "ship_type_id": 587, "items": [{"type_id": 439, "flag": "NotARealFlag", "quantity": 1}]}),
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
        "name": "Brawl Rifter",
        "description": "Fast tackle",
        "ship_type_id": 587,
        "items": [
            {"type_id": 439, "flag": "MedSlot0", "quantity": 1},
            {"type_id": 439, "flag": "Invalid", "quantity": 1}
        ]
    });
    let fit_created = dispatch_character_routes(
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
    assert_eq!(fit_created.status_code, 201);
    let fitting_id = body(&fit_created)["fitting_id"].as_i64().unwrap();

    let fits_get = dispatch_character_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/fittings",
        &p_char,
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert_eq!(fits_get.status_code, 200);
    let created_row = body(&fits_get)
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["fitting_id"] == fitting_id)
        .expect("saved fitting returned");
    assert_eq!(created_row["items"].as_array().unwrap().len(), 1);

    let p_fit = params(&[
        ("character_id", &char_str),
        ("fitting_id", &fitting_id.to_string()),
    ]);
    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "DELETE",
            "/characters/{character_id}/fittings/{fitting_id}",
            &p_fit,
            &empty_q,
            None,
            1,
            b""
        )
        .unwrap()
        .status_code,
        204
    );

    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "GET",
            "/characters/{character_id}/assets",
            &p_char,
            &empty_q,
            None,
            1,
            b""
        )
        .unwrap()
        .status_code,
        200
    );
    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/assets",
            &p_corp,
            &empty_q,
            None,
            1,
            b""
        )
        .unwrap()
        .status_code,
        200
    );
}

#[test]
fn character_routes_errors_assets_coordinates_clones_skills_online_ship_and_fittings() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let empty_p = HashMap::new();
    let empty_q = HashMap::new();
    let p_char = params(&[("character_id", &char_id.to_string())]);
    let p_corp = params(&[("corporation_id", &corp_id.to_string())]);
    let p_missing_char = params(&[("character_id", "90999666")]);
    let p_missing_corp = params(&[("corporation_id", "98999666")]);

    // All GET/POST/DELETE bad path params (400) & 404 missing owner
    for (method, path) in [
        ("GET", "/characters/{character_id}/portrait"),
        ("GET", "/characters/{character_id}/standings"),
        ("GET", "/characters/{character_id}/fatigue"),
        ("GET", "/characters/{character_id}/blueprints"),
        ("GET", "/characters/{character_id}/clones"),
        ("GET", "/characters/{character_id}/implants"),
        ("GET", "/characters/{character_id}/skills"),
        ("GET", "/characters/{character_id}/skillqueue"),
        ("GET", "/characters/{character_id}/location"),
        ("GET", "/characters/{character_id}/online"),
        ("GET", "/characters/{character_id}/ship"),
        ("GET", "/characters/{character_id}/fittings"),
        ("POST", "/characters/{character_id}/fittings"),
        ("DELETE", "/characters/{character_id}/fittings/{fitting_id}"),
        ("GET", "/characters/{character_id}/assets"),
        ("POST", "/characters/{character_id}/assets/locations"),
        ("POST", "/characters/{character_id}/assets/names"),
        ("GET", "/corporations/{corporation_id}/assets"),
        ("POST", "/corporations/{corporation_id}/assets/locations"),
        ("POST", "/corporations/{corporation_id}/assets/names"),
    ] {
        assert_eq!(
            dispatch_character_routes(&ctx, method, path, &empty_p, &empty_q, None, 1, b"[]")
                .unwrap()
                .status_code,
            400,
            "missing path param 400 on {method} {path}"
        );
        let mp = if path.contains("corporation_id") {
            p_missing_corp.clone()
        } else if path.contains("fitting_id") {
            params(&[("character_id", "90999666"), ("fitting_id", "1")])
        } else {
            p_missing_char.clone()
        };
        let body_bytes = if path.ends_with("/fittings") && method == "POST" {
            b"{\"name\":\"Fit\",\"description\":\"D\",\"ship_type_id\":587,\"items\":[{\"type_id\":34,\"flag\":\"Cargo\",\"quantity\":1}]}".as_slice()
        } else {
            b"[1]".as_slice()
        };
        assert_eq!(
            dispatch_character_routes(&ctx, method, path, &mp, &empty_q, None, 1, body_bytes)
                .unwrap()
                .status_code,
            404,
            "404 on {method} {path}"
        );
    }

    // Seed positions, Upwell structures, solar-system items, owner notes, alpha clone skill caps,
    // implants, location variants, login stats, custom ship name via note, and another owner's fitting
    let space_item_id = 885_001_i64;
    let struct_id = 1_000_085_001_i64;
    let struct_item_id = 885_002_i64;
    let contained_item_id = 885_003_i64;
    let noted_ship_id = 885_004_i64;
    let other_char_id = 90_885_001_i64;
    let other_fit_id = 885_101_i64;

    let any_agent_id = *ctx.sde.agents.keys().next().expect("agent");
    let any_faction_id = i64::from(*ctx.sde.factions.keys().next().expect("faction"));
    let any_skill_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 16)
        .map(|t| t.type_id)
        .unwrap_or(3402);

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 1; // Alpha grade exercises alpha_skill_caps
        ch.standings.insert(any_faction_id, 2.0);
        ch.standings.insert(any_agent_id, 3.0);
        ch.standings.insert(999_777_666, 1.0); // unknown -> continue
        ch.jump_fatigue_minutes = 15.0;
        ch.jump_fatigue_until_unix = 0;
        ch.last_clone_jump_unix = 1_700_000_050;
        ch.home_station_id = struct_id;
        ch.structure_id = Some(struct_id);

        let mut och = ch.clone();
        och.character_id = other_char_id;
        och.user_id = 985;
        och.active_ship_id = 999_999_991; // missing ship
        och.structure_id = None;
        och.station_id = Some(struct_id); // >= 100_000_000 -> structure_id
        snap.characters.insert(other_char_id, och);

        snap.character_last_jump_unix
            .insert(char_id, ctx.now_unix().saturating_sub(60));
        snap.character_login_stats.insert(
            char_id,
            eve_db::CharacterLoginStats {
                last_login_unix: 1_700_000_100,
                last_logout_unix: 1_700_000_200,
                logins_count: 4,
            },
        );

        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Asset Cit".to_string(),
                position: [100.0, 200.0, 300.0],
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );
        snap.items.insert(
            space_item_id,
            eve_db::InventoryItemRecord {
                item_id: space_item_id,
                type_id: 587,
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            struct_item_id,
            eve_db::InventoryItemRecord {
                item_id: struct_item_id,
                type_id: 35832,
                owner_id: corp_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            contained_item_id,
            eve_db::InventoryItemRecord {
                item_id: contained_item_id,
                type_id: 34,
                owner_id: char_id,
                location_id: struct_id,
                flag: InventoryFlag::None,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            noted_ship_id,
            eve_db::InventoryItemRecord {
                item_id: noted_ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            885_005,
            eve_db::InventoryItemRecord {
                item_id: 885_005,
                type_id: 9899,
                owner_id: char_id,
                location_id: char_id,
                flag: InventoryFlag::Implant,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            885_006,
            eve_db::InventoryItemRecord {
                item_id: 885_006,
                type_id: 587,
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.owner_notes.insert(
            885_200,
            eve_db::OwnerNoteRecord {
                note_id: 885_200,
                character_id: char_id,
                item_id: noted_ship_id,
                label: "My Noted Ship".to_string(),
                note: String::new(),
                timestamp_unix: 1_700_000_000,
            },
        );
        snap.skills.entry(char_id).or_default().insert(
            any_skill_tid,
            eve_db::TrainedSkillRecord {
                type_id: any_skill_tid,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.saved_fittings.insert(
            other_fit_id,
            eve_db::SavedFittingRecord {
                fitting_id: other_fit_id,
                owner_id: other_char_id,
                name: "Not Mine".to_string(),
                ship_type_id: 587,
                description: String::new(),
                items: vec![(34, InventoryFlag::Cargo, 1)],
            },
        );
    });

    // Put space_item_id ball in solar system 30000142 to cover systems ball lookup in resolve_item_coordinates
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let handle = ctx.systems.get_or_spawn(30_000_142);
        let _ = handle
            .spawn_ball(eve_destiny::Ball {
                id: space_item_id,
                type_id: 587,
                position: glam::DVec3::new(111.0, 222.0, 333.0),
                velocity: glam::DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 1.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await;
    });

    // Assets list + locations + names (both valid and error / 404 cases)
    assert_eq!(
        dispatch_character_routes(&ctx, "GET", "/characters/{character_id}/assets", &p_char, &empty_q, None, 999, b"")
            .unwrap()
            .status_code,
        404
    );
    let char_assets = dispatch_character_routes(&ctx, "GET", "/characters/{character_id}/assets", &p_char, &empty_q, None, 1, b"").unwrap();
    assert_eq!(char_assets.status_code, 200);
    assert!(body(&char_assets).as_array().unwrap().iter().any(|r| r["location_type"] == "solar_system"));

    let locs = dispatch_character_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/assets/locations",
        &p_char,
        &empty_q,
        None,
        1,
        &serde_json::to_vec(&json!([space_item_id, contained_item_id, noted_ship_id])).unwrap(),
    )
    .unwrap();
    assert_eq!(locs.status_code, 200);
    assert_eq!(body(&locs).as_array().unwrap().len(), 3);

    let corp_locs = dispatch_character_routes(
        &ctx,
        "POST",
        "/corporations/{corporation_id}/assets/locations",
        &p_corp,
        &empty_q,
        None,
        1,
        &serde_json::to_vec(&json!([struct_item_id])).unwrap(),
    )
    .unwrap();
    assert_eq!(corp_locs.status_code, 200);

    assert_eq!(
        dispatch_character_routes(&ctx, "POST", "/characters/{character_id}/assets/locations", &p_char, &empty_q, None, 1, b"bad")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_character_routes(&ctx, "POST", "/characters/{character_id}/assets/locations", &p_char, &empty_q, None, 1, b"[999888777]")
            .unwrap()
            .status_code,
        404
    );

    let names = dispatch_character_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/assets/names",
        &p_char,
        &empty_q,
        None,
        1,
        &serde_json::to_vec(&json!([noted_ship_id, space_item_id])).unwrap(),
    )
    .unwrap();
    assert_eq!(names.status_code, 200);
    assert_eq!(body(&names)[0]["name"], "My Noted Ship");
    assert_eq!(
        dispatch_character_routes(&ctx, "POST", "/characters/{character_id}/assets/names", &p_char, &empty_q, None, 1, b"bad")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_character_routes(&ctx, "POST", "/characters/{character_id}/assets/names", &p_char, &empty_q, None, 1, b"[999888777]")
            .unwrap()
            .status_code,
        404
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
            &serde_json::to_vec(&json!([struct_item_id])).unwrap(),
        )
        .unwrap()
        .status_code,
        200
    );

    // Standings, fatigue, clones (structure home + last_clone_jump_date), implants, alpha skills
    assert_eq!(
        dispatch_character_routes(&ctx, "GET", "/characters/{character_id}/standings", &p_char, &empty_q, None, 1, b"")
            .unwrap()
            .status_code,
        200
    );
    let fat = dispatch_character_routes(&ctx, "GET", "/characters/{character_id}/fatigue", &p_char, &empty_q, None, 1, b"").unwrap();
    assert_eq!(fat.status_code, 200);
    assert!(body(&fat).get("jump_fatigue_expire_date").is_some());

    let clones = dispatch_character_routes(&ctx, "GET", "/characters/{character_id}/clones", &p_char, &empty_q, None, 1, b"").unwrap();
    assert_eq!(body(&clones)["home_location"]["location_type"], "structure");
    assert!(body(&clones).get("last_clone_jump_date").is_some());

    let implants = dispatch_character_routes(&ctx, "GET", "/characters/{character_id}/implants", &p_char, &empty_q, None, 1, b"").unwrap();
    assert!(body(&implants).as_array().unwrap().contains(&json!(9899)));

    assert_eq!(
        dispatch_character_routes(&ctx, "GET", "/characters/{character_id}/skills", &p_char, &empty_q, None, 1, b"")
            .unwrap()
            .status_code,
        200
    );

    // Location & online & ship edge cases
    let loc1 = dispatch_character_routes(&ctx, "GET", "/characters/{character_id}/location", &p_char, &empty_q, None, 1, b"").unwrap();
    assert_eq!(body(&loc1)["structure_id"], struct_id);
    let loc2 = dispatch_character_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/location",
        &params(&[("character_id", &other_char_id.to_string())]),
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert_eq!(body(&loc2)["structure_id"], struct_id);

    let onl = dispatch_character_routes(&ctx, "GET", "/characters/{character_id}/online", &p_char, &empty_q, None, 1, b"").unwrap();
    assert!(body(&onl).get("last_logout").is_some());
    assert_eq!(body(&onl)["logins"], 4);

    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "GET",
            "/characters/{character_id}/ship",
            &params(&[("character_id", &other_char_id.to_string())]),
            &empty_q,
            None,
            1,
            b"",
        )
        .unwrap()
        .status_code,
        404
    );

    // Switch char active ship to noted_ship_id to cover owner_notes name fallback
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = noted_ship_id;
    });
    let shp = dispatch_character_routes(&ctx, "GET", "/characters/{character_id}/ship", &p_char, &empty_q, None, 1, b"").unwrap();
    assert_eq!(body(&shp)["ship_name"], "My Noted Ship");

    // POST/DELETE /fittings validation, 404, 403
    for bad_fit in [
        b"bad".as_slice(),
        b"{}",
        b"{\"name\":\"\"}",
        b"{\"name\":\"Fit\"}",
        b"{\"name\":\"Fit\",\"description\":\"D\"}",
        b"{\"name\":\"Fit\",\"description\":\"D\",\"ship_type_id\":-1}",
        b"{\"name\":\"Fit\",\"description\":\"D\",\"ship_type_id\":99999999}",
        b"{\"name\":\"Fit\",\"description\":\"D\",\"ship_type_id\":587}",
        b"{\"name\":\"Fit\",\"description\":\"D\",\"ship_type_id\":587,\"items\":[]}",
        b"{\"name\":\"Fit\",\"description\":\"D\",\"ship_type_id\":587,\"items\":[{\"flag\":\"Cargo\",\"quantity\":1}]}",
        b"{\"name\":\"Fit\",\"description\":\"D\",\"ship_type_id\":587,\"items\":[{\"type_id\":-1,\"flag\":\"Cargo\",\"quantity\":1}]}",
        b"{\"name\":\"Fit\",\"description\":\"D\",\"ship_type_id\":587,\"items\":[{\"type_id\":99999999,\"flag\":\"Cargo\",\"quantity\":1}]}",
        b"{\"name\":\"Fit\",\"description\":\"D\",\"ship_type_id\":587,\"items\":[{\"type_id\":34,\"quantity\":1}]}",
        b"{\"name\":\"Fit\",\"description\":\"D\",\"ship_type_id\":587,\"items\":[{\"type_id\":34,\"flag\":\"BadFlag\",\"quantity\":1}]}",
        b"{\"name\":\"Fit\",\"description\":\"D\",\"ship_type_id\":587,\"items\":[{\"type_id\":34,\"flag\":\"Cargo\"}]}",
        b"{\"name\":\"Fit\",\"description\":\"D\",\"ship_type_id\":587,\"items\":[{\"type_id\":34,\"flag\":\"Cargo\",\"quantity\":0}]}",
        b"{\"name\":\"Fit\",\"description\":\"D\",\"ship_type_id\":587,\"items\":[{\"type_id\":34,\"flag\":\"Invalid\",\"quantity\":1}]}",
    ] {
        assert_eq!(
            dispatch_character_routes(&ctx, "POST", "/characters/{character_id}/fittings", &p_char, &empty_q, None, 1, bad_fit)
                .unwrap()
                .status_code,
            400
        );
    }
    assert_eq!(
        dispatch_character_routes(
            &ctx,
            "DELETE",
            "/characters/{character_id}/fittings/{fitting_id}",
            &params(&[("character_id", &char_id.to_string()), ("fitting_id", "bad")]),
            &empty_q,
            None,
            1,
            b"",
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
            &params(&[("character_id", &char_id.to_string()), ("fitting_id", "9999999")]),
            &empty_q,
            None,
            1,
            b"",
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
            &params(&[("character_id", &char_id.to_string()), ("fitting_id", &other_fit_id.to_string())]),
            &empty_q,
            None,
            1,
            b"",
        )
        .unwrap()
        .status_code,
        403
    );
}
