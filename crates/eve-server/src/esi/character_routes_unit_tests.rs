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
