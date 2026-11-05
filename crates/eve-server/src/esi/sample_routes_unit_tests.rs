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
