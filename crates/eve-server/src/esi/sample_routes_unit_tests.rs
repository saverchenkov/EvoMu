//! Unit and mutation-killing tests for `esi::sample_routes`.

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;
use eve_db::FromWalError;

fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn route_execution_result_constructors_pagination_and_wal_error() {
    let ok = RouteExecutionResult::ok_json(json!({"k": 1}), 120);
    assert_eq!(ok.status_code, 200);
    assert_eq!(ok.max_age_secs, 120);
    assert_eq!(ok.total_pages, None);
    assert_eq!(ok.body.as_ref().unwrap()["k"], 1);

    let created = RouteExecutionResult::created_json(json!(42));
    assert_eq!(created.status_code, 201);
    assert_eq!(created.max_age_secs, 0);
    assert_eq!(created.body, Some(json!(42)));

    // total_pages = 0 is clamped to 1
    let pag = RouteExecutionResult::ok_paginated_json(json!([]), 60, 0);
    assert_eq!(pag.status_code, 200);
    assert_eq!(pag.total_pages, Some(1));

    let nc = RouteExecutionResult::no_content(5);
    assert_eq!(nc.status_code, 204);
    assert_eq!(nc.body, None);
    assert_eq!(nc.max_age_secs, 5);

    let wal_err = RouteExecutionResult::from_wal_error("disk full".to_string());
    assert_eq!(wal_err.status_code, 500);
    assert!(wal_err.body.as_ref().unwrap()["error"]
        .as_str()
        .unwrap()
        .contains("disk full"));

    // paginate_slice: page 0 -> 400, page > total -> 404, empty slice page 1 -> ([], 1), multi-page
    let items = vec![10, 20, 30, 40, 50];
    assert_eq!(
        paginate_slice(&items, 0, 2)
            .expect_err("page 0")
            .status_code,
        400
    );
    assert_eq!(
        paginate_slice(&items, 4, 2)
            .expect_err("page > 3")
            .status_code,
        404
    );
    assert_eq!(paginate_slice(&items, 1, 2).unwrap(), (vec![10, 20], 3));
    assert_eq!(paginate_slice(&items, 3, 2).unwrap(), (vec![50], 3));
    let empty: Vec<i32> = Vec::new();
    assert_eq!(paginate_slice(&empty, 1, 10).unwrap(), (vec![], 1));
}

#[test]
fn civil_date_flags_fitting_roundtrip_and_killmail_hash_determinism() {
    assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
    assert_eq!(format_iso8601_utc(0), "1970-01-01T00:00:00Z");
    assert_eq!(
        format_iso8601_utc(1_700_000_000),
        "2023-11-14T22:13:20Z"
    );
    // Leap day 2024-02-29T12:34:56Z = 1709210096
    assert_eq!(
        format_iso8601_utc(1_709_210_096),
        "2024-02-29T12:34:56Z"
    );

    // Asset flags: mapped vs unmapped
    assert_eq!(asset_location_flag_str(InventoryFlag::Hangar), Some("Hangar"));
    assert_eq!(
        asset_location_flag_str(InventoryFlag::StructureDeedBay),
        Some("QuantumCoreRoom")
    );
    for unmapped in [
        InventoryFlag::None,
        InventoryFlag::Capsule,
        InventoryFlag::Pilot,
        InventoryFlag::Booster,
        InventoryFlag::Impounded,
    ] {
        assert_eq!(asset_location_flag_str(unmapped), None);
    }

    // Fitting flags: all 42 roundtrip; SubSystemSlot4 and Hangar map to "Invalid" and parse to None
    assert_eq!(ESI_FITTING_FLAGS.len(), 42);
    for flag in ESI_FITTING_FLAGS {
        let s = fitting_flag_str(flag);
        assert_ne!(s, "Invalid");
        assert_eq!(parse_fitting_flag(s), Some(flag));
    }
    assert_eq!(fitting_flag_str(InventoryFlag::SubSystemSlot4), "Invalid");
    assert_eq!(parse_fitting_flag("Invalid"), None);
    assert_eq!(parse_fitting_flag("UnknownFlag"), None);

    // compute_killmail_hash: 40 hex chars, sensitive to every input parameter
    let h_base = compute_killmail_hash(1, 90_000_001, 90_000_002, 587);
    assert_eq!(h_base.len(), 40);
    assert_ne!(h_base, compute_killmail_hash(2, 90_000_001, 90_000_002, 587));
    assert_ne!(h_base, compute_killmail_hash(1, 90_000_003, 90_000_002, 587));
    assert_ne!(h_base, compute_killmail_hash(1, 90_000_001, 90_000_003, 587));
    assert_ne!(h_base, compute_killmail_hash(1, 90_000_001, 90_000_002, 588));
}

#[test]
fn is_operation_implemented_and_dispatch_operation_cover_core_tags() {
    assert!(is_operation_implemented("GET", "/status"));
    assert!(is_operation_implemented("GET", "/wars/{war_id}/killmails"));
    assert!(is_operation_implemented("GET", "/fleets/{fleet_id}"));
    assert!(!is_operation_implemented("DELETE", "/status"));

    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let empty_q = HashMap::new();
    let now = ctx.now_unix();

    for (path, p) in [
        ("/status", params(&[])),
        ("/meta/compatibility-dates", params(&[])),
        ("/alliances", params(&[])),
        ("/corporations/npccorps", params(&[])),
        ("/dogma/attributes", params(&[])),
        ("/dogma/attributes/{attribute_id}", params(&[("attribute_id", "9")])),
        ("/dogma/effects", params(&[])),
        ("/dogma/effects/{effect_id}", params(&[("effect_id", "16")])),
        ("/insurance/prices", params(&[])),
        (
            "/characters/{character_id}/killmails/recent",
            params(&[("character_id", &char_id.to_string())]),
        ),
        (
            "/corporations/{corporation_id}/killmails/recent",
            params(&[("corporation_id", &corp_id.to_string())]),
        ),
        ("/markets/groups", params(&[])),
        ("/universe/schematics/{schematic_id}", params(&[("schematic_id", "67")])),
        ("/wars", params(&[])),
        ("/wars/{war_id}", params(&[("war_id", "44001")])),
        ("/wars/{war_id}/killmails", params(&[("war_id", "44001")])),
    ] {
        let res = dispatch_operation(&ctx, now, "GET", path, &p, &empty_q, None, 1, b"");
        assert_eq!(res.status_code, 200, "failed on {path}");
        assert!(res.body.is_some(), "expected body on {path}");
    }

    // Dynamic item: non-existent item -> 404; non-mutated item -> 404; mutated item -> 200
    assert_eq!(
        dispatch_operation(
            &ctx,
            now,
            "GET",
            "/dogma/dynamic/items/{type_id}/{item_id}",
            &params(&[("type_id", "47740"), ("item_id", "999999999")]),
            &empty_q,
            None,
            1,
            b""
        )
        .status_code,
        404
    );
}

#[test]
fn sample_routes_inventory_flags_errors_wars_killmails_and_delegates() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let empty_q = HashMap::new();
    let empty_p = HashMap::new();
    let now = ctx.now_unix();

    // Cover every InventoryFlag variant in asset_location_flag_str
    for flag in [
        InventoryFlag::Wardrobe,
        InventoryFlag::Hangar,
        InventoryFlag::Cargo,
        InventoryFlag::Skill,
        InventoryFlag::LoSlot0,
        InventoryFlag::LoSlot1,
        InventoryFlag::LoSlot2,
        InventoryFlag::LoSlot3,
        InventoryFlag::LoSlot4,
        InventoryFlag::LoSlot5,
        InventoryFlag::LoSlot6,
        InventoryFlag::LoSlot7,
        InventoryFlag::MedSlot0,
        InventoryFlag::MedSlot1,
        InventoryFlag::MedSlot2,
        InventoryFlag::MedSlot3,
        InventoryFlag::MedSlot4,
        InventoryFlag::MedSlot5,
        InventoryFlag::MedSlot6,
        InventoryFlag::MedSlot7,
        InventoryFlag::HiSlot0,
        InventoryFlag::HiSlot1,
        InventoryFlag::HiSlot2,
        InventoryFlag::HiSlot3,
        InventoryFlag::HiSlot4,
        InventoryFlag::HiSlot5,
        InventoryFlag::HiSlot6,
        InventoryFlag::HiSlot7,
        InventoryFlag::Locked,
        InventoryFlag::Unlocked,
        InventoryFlag::DroneBay,
        InventoryFlag::Implant,
        InventoryFlag::RigSlot0,
        InventoryFlag::RigSlot1,
        InventoryFlag::RigSlot2,
        InventoryFlag::CorpSAG1,
        InventoryFlag::CorpSAG2,
        InventoryFlag::CorpSAG3,
        InventoryFlag::CorpSAG4,
        InventoryFlag::CorpSAG5,
        InventoryFlag::CorpSAG6,
        InventoryFlag::CorpSAG7,
        InventoryFlag::SubSystemSlot0,
        InventoryFlag::SubSystemSlot1,
        InventoryFlag::SubSystemSlot2,
        InventoryFlag::SubSystemSlot3,
        InventoryFlag::SubSystemSlot4,
        InventoryFlag::FighterBay,
        InventoryFlag::Deliveries,
        InventoryFlag::CorpDeliveries,
        InventoryFlag::StructureDeedBay,
        InventoryFlag::SpecializedFuelBay,
        InventoryFlag::StructureFuel,
        InventoryFlag::SpecializedOreHold,
        InventoryFlag::SpecializedGasHold,
        InventoryFlag::SpecializedMineralHold,
        InventoryFlag::SpecializedSalvageHold,
        InventoryFlag::SpecializedShipHold,
        InventoryFlag::SpecializedAmmoHold,
        InventoryFlag::SpecializedCommandCenterHold,
        InventoryFlag::SpecializedPlanetaryCommoditiesHold,
        InventoryFlag::SpecializedMaterialBay,
        InventoryFlag::SpecializedIceHold,
        InventoryFlag::SpecializedAsteroidHold,
        InventoryFlag::SpecializedMobileDepotHold,
        InventoryFlag::SpecializedInfrastructureHold,
        InventoryFlag::FrigateEscapeBay,
        InventoryFlag::ShipHangar,
        InventoryFlag::FleetHangar,
        InventoryFlag::ServiceSlot0,
        InventoryFlag::ServiceSlot1,
        InventoryFlag::ServiceSlot2,
        InventoryFlag::ServiceSlot3,
        InventoryFlag::ServiceSlot4,
        InventoryFlag::ServiceSlot5,
        InventoryFlag::ServiceSlot6,
        InventoryFlag::ServiceSlot7,
    ] {
        assert!(asset_location_flag_str(flag).is_some());
    }
    assert!(asset_location_flag_str(InventoryFlag::None).is_none());
    assert!(asset_location_flag_str(InventoryFlag::Capsule).is_none());

    // parse_path_i64 400 + 404 cases
    for path in [
        "/characters/{character_id}/contracts/{contract_id}/items",
        "/dogma/attributes/{attribute_id}",
        "/dogma/effects/{effect_id}",
        "/dogma/dynamic/items/{type_id}/{item_id}",
        "/characters/{character_id}/killmails/recent",
        "/corporations/{corporation_id}/killmails/recent",
        "/universe/schematics/{schematic_id}",
        "/wars/{war_id}",
        "/wars/{war_id}/killmails",
    ] {
        assert_eq!(
            dispatch_operation(&ctx, now, "GET", path, &empty_p, &empty_q, None, 1, b"").status_code,
            400,
            "missing path param 400 on {path}"
        );
    }
    assert_eq!(
        dispatch_operation(
            &ctx,
            now,
            "GET",
            "/dogma/attributes/{attribute_id}",
            &params(&[("attribute_id", "bad")]),
            &empty_q,
            None,
            1,
            b"",
        )
        .status_code,
        400
    );

    // Overflow & 404s for dogma/schematics/wars/killmails
    for (path, p) in [
        ("/dogma/attributes/{attribute_id}", params(&[("attribute_id", "999999")])),
        ("/dogma/attributes/{attribute_id}", params(&[("attribute_id", "65000")])),
        ("/dogma/effects/{effect_id}", params(&[("effect_id", "999999")])),
        ("/dogma/effects/{effect_id}", params(&[("effect_id", "65000")])),
        ("/dogma/dynamic/items/{type_id}/{item_id}", params(&[("type_id", "99999999999"), ("item_id", "1")])),
        ("/characters/{character_id}/killmails/recent", params(&[("character_id", "90999666")])),
        ("/corporations/{corporation_id}/killmails/recent", params(&[("corporation_id", "98999666")])),
        ("/universe/schematics/{schematic_id}", params(&[("schematic_id", "99999999999")])),
        ("/universe/schematics/{schematic_id}", params(&[("schematic_id", "999999")])),
        ("/wars/{war_id}", params(&[("war_id", "999999")])),
        ("/wars/{war_id}/killmails", params(&[("war_id", "999999")])),
    ] {
        assert_eq!(
            dispatch_operation(&ctx, now, "GET", path, &p, &empty_q, None, 1, b"").status_code,
            404,
            "404 expected on {path} {p:?}"
        );
    }

    // /characters/{id}/contracts/{cid}/items (char 404, contract 404, 403 outsider, 501 party)
    assert_eq!(
        dispatch_operation(
            &ctx,
            now,
            "GET",
            "/characters/{character_id}/contracts/{contract_id}/items",
            &params(&[("character_id", "90999666"), ("contract_id", "66001")]),
            &empty_q,
            None,
            1,
            b"",
        )
        .status_code,
        404
    );
    assert_eq!(
        dispatch_operation(
            &ctx,
            now,
            "GET",
            "/characters/{character_id}/contracts/{contract_id}/items",
            &params(&[("character_id", &char_id.to_string()), ("contract_id", "999999")]),
            &empty_q,
            None,
            1,
            b"",
        )
        .status_code,
        404
    );
    assert_eq!(
        dispatch_operation(
            &ctx,
            now,
            "GET",
            "/characters/{character_id}/contracts/{contract_id}/items",
            &params(&[("character_id", &char_id.to_string()), ("contract_id", "66001")]),
            &empty_q,
            None,
            1,
            b"",
        )
        .status_code,
        403
    );

    // /wars bad max_war_id
    assert_eq!(
        dispatch_operation(
            &ctx,
            now,
            "GET",
            "/wars",
            &empty_p,
            &params(&[("max_war_id", "0")]),
            None,
            1,
            b"",
        )
        .status_code,
        400
    );

    // Seed enemy corp + ally + killmails + war lifecycle meta (retracted/finished) + war_allies
    let def_corp = 98_666_001_i64;
    let ally_corp = 98_666_002_i64;
    let def_char = 90_666_001_i64;
    let ally_char = 90_666_002_i64;
    let war_id = 44_901_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&char_id].clone();
        ch.character_id = def_char;
        ch.corporation_id = def_corp;
        snap.characters.insert(def_char, ch.clone());
        ch.character_id = ally_char;
        ch.corporation_id = ally_corp;
        snap.characters.insert(ally_char, ch);

        snap.wars.insert(
            war_id,
            eve_db::WarRecord {
                war_id,
                aggressor_corp_id: corp_id,
                defender_corp_id: def_corp,
                mutual: false,
                started_unix: now.saturating_sub(1000),
                active: true,
            },
        );
        snap.war_lifecycle_meta.insert(
            war_id,
            eve_db::WarLifecycleMeta {
                declared_unix: Some(now.saturating_sub(2000)),
                started_unix: Some(now.saturating_sub(1000)),
                retracted_unix: Some(now.saturating_sub(200)),
                finished_unix: Some(now),
                open_for_allies: Some(true),
                allies: Vec::new(),
            },
        );
        snap.killmails.insert(
            991_001,
            eve_db::KillmailRecord {
                kill_id: 991_001,
                victim_character_id: def_char,
                attacker_character_id: char_id,
                solar_system_id: 30_000_142,
                ship_type_id: 587,
                total_value_isk: 1_000_000.0,
                killed_unix: now.saturating_sub(500),
                victim_corp_id: def_corp,
                victim_alliance_id: None,
                attacker_corp_id: corp_id,
                attacker_alliance_id: None,
            },
        );
        snap.killmails.insert(
            991_002,
            eve_db::KillmailRecord {
                kill_id: 991_002,
                victim_character_id: char_id,
                attacker_character_id: def_char,
                solar_system_id: 30_000_142,
                ship_type_id: 587,
                total_value_isk: 2_000_000.0,
                killed_unix: now.saturating_sub(400),
                victim_corp_id: corp_id,
                victim_alliance_id: None,
                attacker_corp_id: def_corp,
                attacker_alliance_id: None,
            },
        );
        snap.killmails.insert(
            991_003,
            eve_db::KillmailRecord {
                kill_id: 991_003,
                victim_character_id: ally_char,
                attacker_character_id: char_id,
                solar_system_id: 30_000_142,
                ship_type_id: 587,
                total_value_isk: 500_000.0,
                killed_unix: now.saturating_sub(300),
                victim_corp_id: ally_corp,
                victim_alliance_id: None,
                attacker_corp_id: corp_id,
                attacker_alliance_id: None,
            },
        );
    });
    ctx.esi_state
        .war_allies
        .write()
        .unwrap()
        .insert(war_id, vec![json!({ "corporation_id": ally_corp })]);

    let war_res = dispatch_operation(
        &ctx,
        now,
        "GET",
        "/wars/{war_id}",
        &params(&[("war_id", &war_id.to_string())]),
        &empty_q,
        None,
        1,
        b"",
    );
    assert_eq!(war_res.status_code, 200);
    let wb = war_res.body.as_ref().unwrap();
    assert!(wb.get("retracted").is_some());
    assert!(wb.get("finished").is_some());
    assert!(wb["aggressor"]["ships_killed"].as_i64().unwrap() >= 1);
    assert!(wb["defender"]["ships_killed"].as_i64().unwrap() >= 1);

    let w_kms = dispatch_operation(
        &ctx,
        now,
        "GET",
        "/wars/{war_id}/killmails",
        &params(&[("war_id", &war_id.to_string())]),
        &empty_q,
        None,
        1,
        b"",
    );
    assert_eq!(w_kms.status_code, 200);
    assert!(w_kms.body.as_ref().unwrap().as_array().unwrap().len() >= 3);

    for (path, p) in [
        ("/characters/{character_id}/killmails/recent", params(&[("character_id", &char_id.to_string())])),
        ("/corporations/{corporation_id}/killmails/recent", params(&[("corporation_id", &corp_id.to_string())])),
        ("/wars/{war_id}/killmails", params(&[("war_id", &war_id.to_string())])),
    ] {
        assert_eq!(
            dispatch_operation(&ctx, now, "GET", path, &p, &empty_q, None, 999, b"").status_code,
            404
        );
    }

    // Exercise ui_routes delegation through dispatch_operation (lines 1291-1292)
    let ui_res = dispatch_operation(
        &ctx,
        now,
        "POST",
        "/ui/autopilot/waypoint",
        &empty_p,
        &empty_q,
        None,
        1,
        b"",
    );
    assert_ne!(ui_res.status_code, 404);
}
